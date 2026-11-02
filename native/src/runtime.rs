//! Thread ownership and source switching. RF hardware is touched only on explicit Start.
use crate::{device::{self,Block,DemoSource,RtlLibrary,RtlSource,BLOCK_SAMPLES},engine::{ActionError,Engine},recording::ReplayReader};
use serde_json::{Value,json};
use std::{path::PathBuf,sync::{Arc,Mutex,MutexGuard,atomic::{AtomicBool,Ordering}},thread::{self,JoinHandle},time::{Duration,Instant}};

fn error(message:impl Into<String>,code:&str)->ActionError{ActionError{message:message.into(),code:code.into(),impacted_vfos:Vec::new()}}
fn guard<T>(mutex:&Mutex<T>)->MutexGuard<'_,T>{mutex.lock().unwrap_or_else(|poison|poison.into_inner())}
fn equivalent(a:&Value,b:&Value)->bool{if a.is_number()&&b.is_number(){a.as_f64()==b.as_f64()}else{a==b}}

enum Source {
    Live(RtlSource),
    Demo(DemoSource),
    Replay{reader:Arc<Mutex<ReplayReader>>,deadline:Instant},
}
impl Source {
    fn read(&mut self,stop:&AtomicBool)->Result<Option<Block>,String>{match self{
        Self::Live(source)=>source.read(),Self::Demo(source)=>Ok(source.read(stop)),
        Self::Replay{reader,deadline}=>{
            let (start,raw,fs,position)={let mut r=guard(reader);let position=r.position_samples;
                let next=r.captures.iter().filter_map(|c|c["core:sample_start"].as_u64()).filter(|n|*n>position).min().unwrap_or(r.total_samples);
                let count=BLOCK_SAMPLES.min(next.saturating_sub(position)as usize);
                if count==0{return Err("REPLAY_END".into())}
                let global=r.global_sample_for(position);let(_,raw)=r.read_chunk(count)?;(global,raw,r.sample_rate,position)};
            if raw.is_empty(){return Err("REPLAY_END".into())}
            *deadline+=Duration::from_secs_f64(raw.len()as f64/2.0/fs);
            // A stop while pacing must not consume the unprocessed block.
            while Instant::now()<*deadline{if stop.load(Ordering::Acquire){guard(reader).seek_samples(position)?;return Ok(None)}thread::sleep(Duration::from_millis(1));}
            Ok(Some(Block{start,raw}))
        }
    }}
    /// Returns a read but unprocessed block to Replay; live/demo streams cannot rewind.
    fn unread(&self,block:&Block)->Result<(),String>{if let Self::Replay{reader,..}=self{let mut r=guard(reader);let position=r.position_samples.saturating_sub(block.raw.len()as u64/2);r.seek_samples(position)?;}Ok(())}
    fn close(&mut self)->Result<(),String>{match self{Self::Live(source)=>source.close(),_=>Ok(())}}
}

pub struct Runtime {
    pub project:PathBuf,
    pub shutdown_requested:AtomicBool,
    pub engine:Arc<Mutex<Engine>>,
    worker:Mutex<Option<JoinHandle<()>>>,
    signal:Mutex<Arc<AtomicBool>>,
    stop_reason:Arc<Mutex<String>>,
    action_gate:Mutex<()>,
    library:Arc<Mutex<Option<Arc<RtlLibrary>>>>,
    replay_reader:Mutex<Option<Arc<Mutex<ReplayReader>>>>,
    live_input:Mutex<Option<Value>>,
    live_blocked:Arc<Mutex<Option<String>>>,
}
impl Runtime {
    pub fn new(project:PathBuf)->Result<Arc<Self>,String>{
        let project=project.canonicalize().map_err(|e|e.to_string())?;
        let mut engine=Engine::new(project.clone())?;
        let loaded=RtlLibrary::load(&device::dll_path(&project));
        let library=match loaded{Ok(lib)=>{let devices=lib.devices();engine.device=json!({"connected":!devices.is_empty(),"name":devices.first().and_then(|v|v["name"].as_str()).unwrap_or("RTL-SDR"),"devices":devices,"error":null});Some(lib)},Err(e)=>{engine.device=json!({"connected":false,"name":"RTL-SDR","error":e});None}};
        let runtime=Arc::new(Self{project,shutdown_requested:AtomicBool::new(false),engine:Arc::new(Mutex::new(engine)),worker:Mutex::new(None),signal:Mutex::new(Arc::new(AtomicBool::new(false))),stop_reason:Arc::new(Mutex::new("manual".into())),action_gate:Mutex::new(()),library:Arc::new(Mutex::new(library)),replay_reader:Mutex::new(None),live_input:Mutex::new(None),live_blocked:Arc::new(Mutex::new(None))});
        let weak=Arc::downgrade(&runtime);
        thread::Builder::new().name("device-presence".into()).spawn(move||loop{
            thread::sleep(Duration::from_secs(2));let Some(r)=weak.upgrade()else{break};if r.shutdown_requested.load(Ordering::Relaxed){break}
            let mut library=guard(&r.library);if library.is_none(){if let Ok(lib)=RtlLibrary::load(&device::dll_path(&r.project)){*library=Some(lib);}}
            if let Some(lib)=library.as_ref(){let devices=lib.devices();let mut engine=guard(&r.engine);let was=engine.device["connected"].as_bool().unwrap_or(false);let connected=!devices.is_empty();engine.device["connected"]=json!(connected);engine.device["devices"]=json!(devices);if connected{if engine.source=="live"{engine.device["name"]=engine.device["devices"][0]["name"].clone();}if !was&&guard(&r.live_blocked).is_none(){engine.device["error"]=Value::Null;}}}
        }).map_err(|e|e.to_string())?;
        Ok(runtime)
    }
    pub fn state(&self)->Value{let mut state=guard(&self.engine).state();state["backend"]=json!("Rust");state["version"]=json!(env!("CARGO_PKG_VERSION"));if let Some(reason)=guard(&self.live_blocked).as_ref(){state["device"]["recovery_required"]=json!(true);state["device"]["recovery_reason"]=json!(reason);}state}
    pub fn frame(&self)->Value{guard(&self.engine).frame()}
    pub fn audio(&self,after:u64)->Value{guard(&self.engine).audio(after)}
    pub fn files(&self)->Value{guard(&self.engine).files()}
    fn response(&self,result:Value)->Value{json!({"ok":true,"state":self.state(),"result":result})}

    fn start_internal(&self,source_name:&str)->Result<(),ActionError>{
        if source_name=="live"&&guard(&self.live_blocked).is_some(){return Err(error("前のUSB受信の停止を確認できません。アプリを終了し、機器状態を確認してから再起動してください。","DEVICE_BUSY"))}
        if guard(&self.engine).running{return Ok(())}
        if let Some(old)=guard(&self.worker).take(){let _=old.join();}
        let signal=Arc::new(AtomicBool::new(false));*guard(&self.signal)=signal.clone();*guard(&self.stop_reason)="manual".into();
        let input=guard(&self.engine).input.clone();
        let (mut source,metadata)=match source_name{
            "live"=>{let library=guard(&self.library).clone().ok_or_else(||error("RTL-SDRのDLLを読み込めません。ドライバー準備の記録を確認してください。","DEVICE"))?;let source=RtlSource::open(library,&input,signal.clone()).map_err(|e|error(e,"DEVICE"))?;let metadata=source.metadata.clone();(Source::Live(source),metadata)},
            "demo"=>{(Source::Demo(DemoSource::new(&input)),json!({"name":"模擬IQ（実機ではありません）","sample_rate":input["sample_rate"],"center_hz":input["center_hz"],"synthetic":true,"gain_db_actual":null,"available_gains_db":[],"stream_start_utc":chrono::Utc::now().to_rfc3339(),"stream_clock_accuracy":"host_synthetic_clock"}))},
            "replay"=>{let reader=guard(&self.replay_reader).clone().ok_or_else(||error("再生するIQファイルを選択してください。","REPLAY"))?;
                // Start after the end replays from the beginning; a mid-file stop resumes in place.
                let ended={let r=guard(&reader);(r.position_samples>=r.total_samples).then(||r.global_sample_for(0))};
                if let Some(global)=ended{guard(&self.engine).reset_for_seek(global).map_err(|e|error(e,"REPLAY"))?;guard(&reader).seek_samples(0).map_err(|e|error(e,"REPLAY"))?;}
                let metadata={let r=guard(&reader);json!({"name":"保存IQ","sample_rate":r.sample_rate,"center_hz":r.center_hz,"input_metadata":r.input_metadata,"stream_start_utc":r.input_metadata.get("stream_start_utc"),"replay_path":r.meta_path.to_string_lossy()})};(Source::Replay{reader,deadline:Instant::now()},metadata)},
            _=>return Err(error("受信ソースが不正です。","SOURCE")),
        };
        guard(&self.engine).on_start(source_name,metadata).map_err(|e|error(e,"START"))?;
        let engine=self.engine.clone();let reason=self.stop_reason.clone();let name=source_name.to_owned();let live_blocked=self.live_blocked.clone();
        let worker=thread::Builder::new().name("spectrum-dsp".into()).spawn(move||{
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->Result<(),String>{
                while !signal.load(Ordering::Acquire){
                    if let Some(block)=source.read(&signal)?{
                        if signal.load(Ordering::Acquire){source.unread(&block)?;break}
                        let replay_position=match &source{Source::Replay{reader,..}=>Some(guard(reader).position_seconds()),_=>None};
                        let started=Instant::now();let mut e=guard(&engine);e.process_block(block.start,&block.raw)?;
                        let elapsed=started.elapsed().as_secs_f64()*1000.0;let block_ms=block.raw.len()as f64/2.0/e.input["sample_rate"].as_f64().unwrap_or(2_048_000.)*1000.0;
                        e.performance["block_latency_ms"]=json!(elapsed);e.performance["block_ms"]=json!(block_ms);
                        if let Some(position)=replay_position{e.replay["position_seconds"]=json!(position);}
                    }
                }Ok(())
            }));
            let problem=match result{Ok(Ok(()))=>None,Ok(Err(e))=>Some(e),Err(_)=>Some("受信処理で内部エラーが発生しました。保存済み部分と記録を確認してください。".into())};
            let close_error=source.close().err();let mut e=guard(&engine);
            let stop_reason=match problem.as_deref(){Some("REPLAY_END")=>"replay_end".to_owned(),Some(message)=>{e.device["error"]=json!(message);if name=="live"{e.device["connected"]=json!(false);} "source_error".to_owned()},None=>guard(&reason).clone()};
            if let Some(message)=close_error{*guard(&live_blocked)=Some(message.clone());e.device["error"]=json!(message);e.device["recovery_required"]=json!(true);}
            e.on_stop(&stop_reason);
        }).map_err(|e|{guard(&self.engine).on_stop("start_error");error(e.to_string(),"THREAD")})?;
        *guard(&self.worker)=Some(worker);Ok(())
    }
    fn stop_internal(&self,reason:&str){
        *guard(&self.stop_reason)=reason.into();guard(&self.signal).store(true,Ordering::Release);
        if let Some(worker)=guard(&self.worker).take(){let _=worker.join();}else{guard(&self.engine).on_stop(reason);}
    }
    fn ensure_source_switch(&self)->Result<(),ActionError>{
        let state=guard(&self.engine).state();let wav=state["vfos"].as_array().is_some_and(|v|v.iter().any(|x|x["wav_recording"].as_bool().unwrap_or(false)));
        if state["trigger"]["armed"].as_bool().unwrap_or(false)||state["recording"]["iq_active"].as_bool().unwrap_or(false)||wav{return Err(error("記録とトリガ待機を終了してからソースを切り替えてください。","INPUT_LOCKED"))}Ok(())
    }
    pub fn action(&self,mut body:Value)->Result<Value,ActionError>{
        if body["action"]=="set_input"&&body.get("input").is_none()&&body["patch"].is_object(){body["input"]=body["patch"].clone();}
        let _gate=guard(&self.action_gate);let action=body["action"].as_str().ok_or_else(||error("操作名がありません。","ACTION"))?;
        match action{
            "start"=>{let requested=body["source"].as_str().unwrap_or("live");let source=if guard(&self.engine).source=="replay"&&requested=="live"&&guard(&self.replay_reader).is_some(){"replay"}else{requested};self.start_internal(source)?;Ok(self.response(Value::Null))},
            "stop"=>{self.stop_internal("manual");Ok(self.response(Value::Null))},
            "set_input"=>{
                let patch=body.get("input").or_else(||body.get("patch")).cloned().unwrap_or(json!({}));
                let confirmed=body["confirm_impacted"].as_bool().unwrap_or(false);
                let (candidate,old,running,source)={let e=guard(&self.engine);(e.plan_input(&patch,confirmed)?,e.input.clone(),e.running,e.source.clone())};
                let changed=patch.as_object().is_some_and(|fields|fields.keys().any(|key|!equivalent(&old[key],&candidate[key])));
                if !changed{return Ok(self.response(Value::Null))}
                if running{self.stop_internal("input_change");}
                let applied=guard(&self.engine).apply(&body);
                let result=match applied{Ok(value)=>value,Err(problem)=>{if running{guard(&self.engine).on_stop("input_restart_error");}return Err(problem)}};
                if running{if let Err(problem)=self.start_internal(&source){guard(&self.engine).on_stop("input_restart_error");return Err(problem)}}
                Ok(self.response(result))
            },
            "replay_open"=>{
                self.ensure_source_switch()?;
                let text=body["path"].as_str().ok_or_else(||error("IQファイルを選択してください。","PATH"))?;
                let path=if std::path::Path::new(text).is_absolute(){PathBuf::from(text)}else{self.project.join(text)}.canonicalize().map_err(|e|error(e.to_string(),"PATH"))?;
                if !path.starts_with(&self.project){return Err(error("このアプリのフォルダー内にあるIQを選択してください。","PATH"))}
                let reader=ReplayReader::new(&path).map_err(|e|error(e,"REPLAY"))?;
                let metadata=json!({"path":reader.meta_path.to_string_lossy(),"duration_seconds":reader.duration_seconds(),"position_seconds":0.0,"sample_rate":reader.sample_rate,"center_hz":reader.center_hz,"warnings":reader.warnings});
                self.stop_internal("source_change");
                {let mut e=guard(&self.engine);if e.source!="replay"{*guard(&self.live_input)=Some(e.input.clone());}e.input["sample_rate"]=json!(reader.sample_rate);e.input["center_hz"]=json!(reader.center_hz);e.source="replay".into();e.replay=metadata;e.reset_for_seek(0).map_err(|e|error(e,"REPLAY"))?;}
                *guard(&self.replay_reader)=Some(Arc::new(Mutex::new(reader)));Ok(self.response(Value::Null))
            },
            "replay_seek"=>{
                self.ensure_source_switch()?;let reader=guard(&self.replay_reader).clone().ok_or_else(||error("再生ファイルがありません。","REPLAY"))?;
                let seconds=body["seconds"].as_f64().ok_or_else(||error("再生位置が不正です。","VALUE"))?;
                if !seconds.is_finite()||seconds<0.0{return Err(error("再生位置は0秒以上で指定してください。","VALUE"))}
                let running=guard(&self.engine).running;if running{self.stop_internal("seek");}
                let sample=guard(&reader).seek_seconds(seconds).map_err(|e|error(e,"REPLAY"))?;
                {let (global,position)={let r=guard(&reader);(r.global_sample_for(sample),r.position_seconds())};let mut e=guard(&self.engine);e.reset_for_seek(global).map_err(|e|error(e,"REPLAY"))?;e.replay["position_seconds"]=json!(position);}
                if running{self.start_internal("replay")?;}Ok(self.response(Value::Null))
            },
            "replay_close"=>{
                self.ensure_source_switch()?;self.stop_internal("source_change");*guard(&self.replay_reader)=None;
                {let mut e=guard(&self.engine);if let Some(input)=guard(&self.live_input).take(){e.input=input;}e.source="live".into();e.replay=json!({});e.reset_for_seek(0).map_err(|e|error(e,"REPLAY"))?;}
                Ok(self.response(Value::Null))
            },
            "shutdown"=>{self.stop_internal("shutdown");self.shutdown_requested.store(true,Ordering::Release);Ok(self.response(Value::Null))},
            _=>{let result=guard(&self.engine).apply(&body)?;Ok(self.response(result))}
        }
    }
    pub fn shutdown(&self){let _gate=guard(&self.action_gate);self.stop_internal("shutdown");self.shutdown_requested.store(true,Ordering::Release);}
}

impl Drop for Runtime{fn drop(&mut self){self.shutdown_requested.store(true,Ordering::Release);self.stop_internal("application_exit");}}

#[cfg(test)]mod tests{
    use super::*;
    fn folder()->PathBuf{let root=std::env::temp_dir().join(format!("spectrum-runtime-{}-{}",std::process::id(),chrono::Utc::now().timestamp_nanos_opt().unwrap()));std::fs::create_dir_all(&root).unwrap();root}
    fn cleanup(path:&std::path::Path){assert!(path.starts_with(std::env::temp_dir()));assert!(path.file_name().unwrap().to_string_lossy().starts_with("spectrum-runtime-"));std::fs::remove_dir_all(path).unwrap();}
    #[test]fn replay_preserves_original_source_gap_boundaries(){
        let root=folder();let mut writer=crate::recording::IQRecorder::new(&root,1000.,1_000_000.,json!({}),10000,10.).unwrap();writer.start(&[]).unwrap();writer.write(&[128;8],100).unwrap();writer.write(&[129;4],110).unwrap();let saved=writer.stop("test");
        let reader=Arc::new(Mutex::new(ReplayReader::new(saved["meta_path"].as_str().unwrap()).unwrap()));let mut source=Source::Replay{reader:reader.clone(),deadline:Instant::now()};let flag=AtomicBool::new(false);
        let first=source.read(&flag).unwrap().unwrap();let second=source.read(&flag).unwrap().unwrap();assert_eq!((first.start,first.raw.len()),(100,8));assert_eq!((second.start,second.raw.len()),(110,4));assert!((guard(&reader).position_seconds()-0.006).abs()<1e-9);drop(source);drop(reader);drop(writer);cleanup(&root);
    }
    #[test]fn replay_stop_while_pacing_or_after_read_keeps_every_sample(){
        let root=folder();let total=2*BLOCK_SAMPLES+1000;let data:Vec<u8>=(0..total*2).map(|i|(i%251)as u8).collect();
        let mut writer=crate::recording::IQRecorder::new(&root,2_048_000.,1_000_000.,json!({}),10_000_000,10.).unwrap();writer.start(&[]).unwrap();writer.write(&data,0).unwrap();let saved=writer.stop("test");
        let reader=Arc::new(Mutex::new(ReplayReader::new(saved["meta_path"].as_str().unwrap()).unwrap()));
        let stopped=AtomicBool::new(true);let running=AtomicBool::new(false);
        // Deadline far ahead: the stop is always seen while pacing.
        let mut paced=Source::Replay{reader:reader.clone(),deadline:Instant::now()+Duration::from_secs(3600)};
        assert!(paced.read(&stopped).unwrap().is_none());assert_eq!(guard(&reader).position_samples,0);
        let mut source=Source::Replay{reader:reader.clone(),deadline:Instant::now()};
        let first=source.read(&running).unwrap().unwrap();source.unread(&first).unwrap();assert_eq!(guard(&reader).position_samples,0);
        let mut source=Source::Replay{reader:reader.clone(),deadline:Instant::now()};let mut seen=Vec::new();
        loop{match source.read(&running){Ok(Some(block))=>{assert_eq!(block.start,seen.len()as u64/2);seen.extend(block.raw)},Err(e)if e=="REPLAY_END"=>break,other=>panic!("{:?}",other.map(|b|b.map(|b|b.start)))}}
        assert_eq!(seen.len(),total*2);assert!(seen==data);drop(paced);drop(source);drop(reader);drop(writer);cleanup(&root);
    }
    fn replay_runtime(root:&std::path::Path,samples:usize)->(Arc<Runtime>,Arc<Mutex<ReplayReader>>){
        let data:Vec<u8>=(0..samples*2).map(|i|(i%251)as u8).collect();let mut writer=crate::recording::IQRecorder::new(root,2_048_000.,1_000_000.,json!({}),100_000_000,10.).unwrap();writer.start(&[]).unwrap();writer.write(&data,0).unwrap();let saved=writer.stop("test");
        let runtime=Runtime::new(root.to_path_buf()).unwrap();runtime.action(json!({"action":"replay_open","path":saved["meta_path"]})).unwrap();let reader=guard(&runtime.replay_reader).clone().unwrap();(runtime,reader)
    }
    fn wait_stopped(runtime:&Runtime){let until=Instant::now()+Duration::from_secs(20);while runtime.state()["running"].as_bool().unwrap(){assert!(Instant::now()<until,"replay did not stop");thread::sleep(Duration::from_millis(5));}}
    #[test]fn replay_start_after_end_rewinds_and_reads_every_sample(){
        let root=folder();let total=2*BLOCK_SAMPLES+1000;let (runtime,reader)=replay_runtime(&root,total);
        let ended=|r:&Runtime|r.state()["notifications"].as_array().unwrap().iter().filter(|n|n["message"].as_str().is_some_and(|m|m.contains("最後まで再生しました"))).count();
        for run in 1..=2{
            runtime.action(json!({"action":"start"})).unwrap();wait_stopped(&runtime);let state=runtime.state();
            assert!(state["device"]["error"].is_null(),"{}",state["device"]["error"]);assert_eq!(guard(&reader).position_samples,total as u64);
            assert_eq!(state["performance"]["input_samples"],total);assert!((state["replay"]["position_seconds"].as_f64().unwrap()-total as f64/2_048_000.).abs()<1e-9);assert_eq!(ended(&runtime),run);
        }
        runtime.shutdown();drop(reader);drop(runtime);cleanup(&root);
    }
    #[test]fn replay_start_after_mid_stop_resumes_in_place(){
        let root=folder();let total=4*BLOCK_SAMPLES;let (runtime,reader)=replay_runtime(&root,total);
        runtime.action(json!({"action":"replay_seek","seconds":2.0*BLOCK_SAMPLES as f64/2_048_000.})).unwrap();let resume=guard(&reader).position_samples;assert!(resume>BLOCK_SAMPLES as u64&&resume<total as u64);
        runtime.action(json!({"action":"start"})).unwrap();runtime.action(json!({"action":"stop"})).unwrap();
        let position=guard(&reader).position_samples;assert!(position>=resume&&position<total as u64,"{position} vs {resume}");
        assert!(runtime.state()["replay"]["position_seconds"].as_f64().unwrap()>=resume as f64/2_048_000.-1e-9);
        runtime.shutdown();drop(reader);drop(runtime);cleanup(&root);
    }
    fn gapped_replay(root:&std::path::Path,first:usize,skip:u64,second:usize)->Arc<Mutex<ReplayReader>>{
        let mut writer=crate::recording::IQRecorder::new(root,2_048_000.,1_000_000.,json!({}),100_000_000,10.).unwrap();writer.start(&[]).unwrap();
        writer.write(&vec![128;first*2],0).unwrap();writer.write(&vec![130;second*2],first as u64+skip).unwrap();let saved=writer.stop("test");
        let reader=ReplayReader::new(saved["meta_path"].as_str().unwrap()).unwrap();assert_eq!(reader.captures.len(),2);Arc::new(Mutex::new(reader))
    }
    fn drain_replay(source:&mut Source,e:&mut Engine)->Vec<u64>{
        let flag=AtomicBool::new(false);let mut starts=vec![];
        loop{match source.read(&flag){Ok(Some(block))=>{starts.push(block.start);e.process_block(block.start,&block.raw).unwrap()},Err(x)if x=="REPLAY_END"=>break starts,other=>panic!("{:?}",other.map(|b|b.map(|b|b.start)))}}
    }
    #[test]fn replay_rewind_around_a_capture_gap_keeps_boundary_start_and_gap_detection(){
        let root=folder();let first=BLOCK_SAMPLES+500;let skip=12_345u64;let second=BLOCK_SAMPLES/2;
        let (block,boundary)=(BLOCK_SAMPLES as u64,first as u64);let (resumed,end)=(boundary+skip,boundary+skip+second as u64);
        let reader=gapped_replay(&root,first,skip,second);let running=AtomicBool::new(false);let stopped=AtomicBool::new(true);
        let replay=||Source::Replay{reader:reader.clone(),deadline:Instant::now()};let position=||guard(&reader).position_samples;
        // A block cut short at the capture start rewinds to its own start, never across the boundary.
        guard(&reader).seek_samples(block).unwrap();let mut source=replay();let cut=source.read(&running).unwrap().unwrap();
        assert_eq!((cut.start,cut.raw.len(),position()),(block,1000,boundary));source.unread(&cut).unwrap();assert_eq!(position(),block);
        // Stops at the first block of the second capture leave the reader on the boundary; the re-read keeps its global start.
        guard(&reader).seek_samples(boundary).unwrap();let mut paced=Source::Replay{reader:reader.clone(),deadline:Instant::now()+Duration::from_secs(3600)};
        assert!(paced.read(&stopped).unwrap().is_none());assert_eq!(position(),boundary);
        let mut source=replay();let after=source.read(&running).unwrap().unwrap();assert_eq!((after.start,after.raw.len()),(resumed,second*2));
        source.unread(&after).unwrap();assert_eq!(position(),boundary);assert_eq!(source.read(&running).unwrap().unwrap().start,resumed);
        let mut e=Engine::new(root.clone()).unwrap();let metadata=json!({"sample_rate":2_048_000.,"center_hz":1_000_000.});
        let notices=|e:&Engine|e.state()["notifications"].as_array().unwrap().iter().filter(|n|n["message"].as_str().is_some_and(|m|m.contains(&format!("入力欠落を検出：{skip} samples")))).count();
        // Uninterrupted: the boundary is one gap of `skip` samples.
        guard(&reader).seek_samples(0).unwrap();e.on_start("replay",metadata.clone()).unwrap();
        assert_eq!(drain_replay(&mut replay(),&mut e),vec![0,block,resumed]);assert_eq!((e.performance["gap_samples"].as_u64(),e.performance["input_samples"].as_u64()),(Some(skip),Some(end)));assert_eq!(notices(&e),1);e.on_stop("replay_end");
        // Stop with the last pre-boundary block unread: the resumed run still detects the gap at the boundary.
        guard(&reader).seek_samples(0).unwrap();e.on_start("replay",metadata.clone()).unwrap();let mut source=replay();
        let b=source.read(&running).unwrap().unwrap();e.process_block(b.start,&b.raw).unwrap();let b=source.read(&running).unwrap().unwrap();source.unread(&b).unwrap();e.on_stop("manual");
        e.on_start("replay",metadata.clone()).unwrap();assert_eq!(drain_replay(&mut replay(),&mut e),vec![block,resumed]);
        assert_eq!((e.performance["gap_samples"].as_u64(),e.performance["input_samples"].as_u64()),(Some(skip),Some(end)));assert_eq!(notices(&e),2);e.on_stop("replay_end");
        // Stop with the first post-boundary block unread: the resumed run starts at that capture's global index.
        // on_start resets the DSP and its gap tracking, so the boundary is not reported a second time.
        guard(&reader).seek_samples(0).unwrap();e.on_start("replay",metadata.clone()).unwrap();let mut source=replay();
        for _ in 0..2{let b=source.read(&running).unwrap().unwrap();e.process_block(b.start,&b.raw).unwrap();}
        let b=source.read(&running).unwrap().unwrap();assert_eq!(b.start,resumed);source.unread(&b).unwrap();e.on_stop("manual");assert_eq!(position(),boundary);
        e.on_start("replay",metadata).unwrap();assert_eq!(drain_replay(&mut replay(),&mut e),vec![resumed]);
        assert_eq!((e.performance["gap_samples"].as_u64(),e.performance["input_samples"].as_u64()),(Some(0),Some(end)));assert_eq!(notices(&e),2);
        e.on_stop("replay_end");drop(e);drop(paced);drop(source);drop(reader);cleanup(&root);
    }
    #[test]fn replay_stop_start_cycles_lose_no_block_and_finish_every_sample(){
        let root=folder();let total=12*BLOCK_SAMPLES;let (runtime,reader)=replay_runtime(&root,total);let position=||guard(&reader).position_samples;
        let processed=|r:&Runtime|r.state()["performance"]["input_samples"].as_u64().unwrap();let mut previous=0u64;
        for cycle in 0..4{
            runtime.action(json!({"action":"start"})).unwrap();
            // Even cycles stop at once (mostly while pacing the first block); odd cycles wait for a processed block.
            if cycle%2==1{let until=Instant::now()+Duration::from_secs(20);while processed(&runtime)<=previous{assert!(Instant::now()<until,"cycle {cycle}: no block processed");thread::sleep(Duration::from_millis(2));}}
            runtime.action(json!({"action":"stop"})).unwrap();let state=runtime.state();
            assert!(!state["running"].as_bool().unwrap());assert!(state["device"]["error"].is_null(),"{}",state["device"]["error"]);
            // No read block is dropped: the reader sits at the end of the last processed block, or where the cycle began.
            let done=processed(&runtime);let now=position();assert_eq!(now,if done>0{done}else{previous},"cycle {cycle}");
            assert!(now>=previous&&now<total as u64,"cycle {cycle}: {now}");previous=now;
        }
        assert!(previous>0);runtime.action(json!({"action":"start"})).unwrap();wait_stopped(&runtime);let state=runtime.state();
        assert!(state["device"]["error"].is_null(),"{}",state["device"]["error"]);assert_eq!(state["performance"]["input_samples"],total);assert_eq!(position(),total as u64);
        assert_eq!(state["notifications"].as_array().unwrap().iter().filter(|n|n["message"].as_str().is_some_and(|m|m.contains("最後まで再生しました"))).count(),1);
        runtime.shutdown();drop(reader);drop(runtime);cleanup(&root);
    }
    #[test]fn numeric_noop_alias_keeps_arm_and_usb_failure_latch_rejects_restart(){
        let root=folder();let runtime=Runtime::new(root.clone()).unwrap();{let mut e=guard(&runtime.engine);e.on_start("demo",json!({"sample_rate":2048000.,"center_hz":32256000.})).unwrap();e.apply(&json!({"action":"arm"})).unwrap();}
        runtime.action(json!({"action":"set_input","patch":{"center_hz":32256000}})).unwrap();assert!(runtime.state()["trigger"]["armed"].as_bool().unwrap());assert!(runtime.state()["running"].as_bool().unwrap());runtime.action(json!({"action":"stop"})).unwrap();*guard(&runtime.live_blocked)=Some("test timeout".into());let result=runtime.action(json!({"action":"start","source":"live"}));assert_eq!(result.unwrap_err().code,"DEVICE_BUSY");runtime.shutdown();drop(runtime);cleanup(&root);
    }
}
