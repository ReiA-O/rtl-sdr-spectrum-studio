//! Application state and one acquisition/replay DSP pipeline. Instrument I/O is
//! owned by Runtime; input changes are planned before that source is stopped.
use crate::calibration::{self,Calibration};
use crate::dsp::{self, Analyzer, Config, VfoDemodulator};
use crate::recording::{IQRecorder, PreTriggerBuffer, WavRecorder};
use crate::trigger::TriggerController;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::Utc;
use rustfft::num_complex::Complex32;
use serde_json::{json, Value};
use std::collections::{HashSet,VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

const AUDIO_RATE: u32 = 32000;
const PRE_BYTES: usize = 64 * 1024 * 1024;
const IQ_BYTES: u64 = 512 * 1024 * 1024;
const IQ_SECONDS: f64 = 120.0;

#[derive(Debug)]
pub struct ActionError { pub message: String, pub code: String, pub impacted_vfos: Vec<Value> }
impl ActionError {
    fn new(code:&str,message:impl Into<String>)->Self {Self{message:message.into(),code:code.into(),impacted_vfos:vec![]}}
    fn bad(message:impl Into<String>)->Self {Self::new("invalid_request",message)}
    fn locked(message:impl Into<String>)->Self {Self::new("locked",message)}
}
impl From<String> for ActionError {fn from(s:String)->Self{Self::bad(s)}}
impl From<&str> for ActionError {fn from(s:&str)->Self{Self::bad(s)}}
fn num(value:&Value,key:&str,default:f64)->f64 {value.get(key).and_then(Value::as_f64).unwrap_or(default)}
fn boolean(value:&Value,key:&str,default:bool)->bool {value.get(key).and_then(Value::as_bool).unwrap_or(default)}
fn text<'a>(value:&'a Value,key:&str,default:&'a str)->&'a str {value.get(key).and_then(Value::as_str).unwrap_or(default)}
fn merge(target:&mut Value,patch:&Value){
    if let (Some(to),Some(from))=(target.as_object_mut(),patch.as_object()){
        for(k,v)in from{if v.is_object()&&to.get(k).is_some_and(Value::is_object){merge(to.get_mut(k).unwrap(),v);}else{to.insert(k.clone(),v.clone());}}
    }
}
fn finite(value:f64,label:&str)->Result<f64,ActionError>{if value.is_finite(){Ok(value)}else{Err(ActionError::bad(format!("{label}は有限値が必要です")))}}
fn identifier(value:&Value)->Option<u64>{value.as_u64().or_else(||value.as_str().and_then(|s|s.parse().ok()))}
fn db(value:f64)->f64{10.0*value.max(1e-30).log10()}
fn same(left:&Value,right:&Value)->bool {match(left.as_f64(),right.as_f64()){(Some(a),Some(b))=>a==b,_=>left==right}}

struct Vfo {
    id:u64,frequency_hz:f64,bandwidth_hz:f64,mode:String,enabled:bool,
    stopped_reason:Option<String>,wav_format:String,demod:Option<VfoDemodulator>,
    wav:Option<WavRecorder>,audio_level:f64,sample_count:u64,
}
impl Vfo {
    fn config(&self)->Value{json!({"id":self.id,"frequency_hz":self.frequency_hz,"bandwidth_hz":self.bandwidth_hz,
        "mode":self.mode,"enabled":self.enabled,"stopped_reason":self.stopped_reason,"wav_format":self.wav_format})}
    fn state(&self,selected:u64)->Value{
        let mut value=self.config();value["selected"]=json!(self.id==selected);
        value["wav_recording"]=json!(self.wav.as_ref().is_some_and(|r|r.active));
        value["audio_level"]=json!(self.audio_level);value["sample_count"]=json!(self.sample_count);value
    }
}

pub struct Engine {
    project:PathBuf,
    pub input:Value,
    pub running:bool,
    pub source:String,
    pub replay:Value,
    pub device:Value,
    pub performance:Value,
    pub last_sample:u64,
    analyzer:Analyzer,
    analysis_extra:Value,
    display:Value,
    measurement:Value,
    trigger_config:Value,
    trigger:TriggerController,
    prebuffer:PreTriggerBuffer,
    iq:Option<IQRecorder>,
    iq_triggered:bool,
    iq_start_sample:Option<u64>,
    iq_last:Value,
    vfos:Vec<Vfo>,
    selected:u64,
    next_vfo:u64,
    layout:Value,
    notifications:VecDeque<Value>,
    notification_id:u64,
    audio_chunks:VecDeque<(u64,Vec<u8>)>,
    audio_sequence:u64,
    frozen_markers:Vec<f64>,
    had_blocks:bool,
    trigger_last_frame:Option<u64>,
    pending_input_change:bool,
    overload_streak:u32,
    overload_notice:bool,
    source_metadata:Value,
    calibration:Option<Calibration>,
    requested_level_unit:String,
}

impl Engine {
    pub fn new(project:PathBuf)->Result<Self,String>{
        let input=json!({"center_hz":32_256_000.0,"sample_rate":2_048_000.0,"gain_db":20.7,"agc":false,"ppm":0,"mode":"auto","device_index":0});
        let config=Config::default();
        let trigger_config=json!({"pre_seconds":1.0,"post_seconds":2.0,"mode":"one_shot","threshold_db":-55.0,
            "metric":"peak","hysteresis_db":3.0,"min_duration_s":0.0,"rearm_s":0.2,"logic":"OR","conditions":[],
            "low_hz":31_900_000.0,"high_hz":32_100_000.0,"link_vfo":null});
        let mut engine=Self {project,input,running:false,source:"live".into(),replay:Value::Null,
            device:json!({"name":"RTL-SDR","connected":false}),performance:json!({"load_percent":0.0,"gap_samples":0,"valid_frames":0}),last_sample:0,
            analyzer:Analyzer::new(config),analysis_extra:json!({"fft_mode":"rbw","rbw_hz":750.0}),
            display:json!({"span_hz":1_800_000.0,"ref_level":-20.0,"range_db":100.0,"traces":{"live":true,"max":false,"min":false,"avg":false}}),
            measurement:json!({"low_hz":31_900_000.0,"high_hz":32_100_000.0,"link_vfo":null,"obw_percent":99.0,"marker_count":6,
                "marker_mode":"auto","marker_follow":true,"delta_reference":0,"manual_markers":[],"selected_user_marker_id":null}),
            trigger:TriggerController::new(2_048_000.0,Self::trigger_controller_config(&trigger_config))?,trigger_config,
            prebuffer:PreTriggerBuffer::new(2_048_000.0,PRE_BYTES)?,iq:None,iq_triggered:false,iq_start_sample:None,iq_last:Value::Null,
            vfos:vec![],selected:1,next_vfo:2,layout:json!({"spectrum_ratio":0.6,"vfo_width":340,"bottom_ratio":0.5}),
            notifications:VecDeque::new(),notification_id:0,audio_chunks:VecDeque::new(),audio_sequence:0,
            frozen_markers:vec![],had_blocks:false,trigger_last_frame:None,pending_input_change:false,
            overload_streak:0,overload_notice:false,source_metadata:json!({}),calibration:None,requested_level_unit:"relative".into()};
        engine.vfos.push(engine.make_vfo(&json!({"id":1,"frequency_hz":32_000_000.0,"bandwidth_hz":200_000.0,"mode":"WFM","enabled":true,"wav_format":"PCM16"}),1)?);
        let last=engine.settings_dir().join("last-session.json");
        if last.exists(){
            let restored=fs::read(&last).map_err(|e|e.to_string()).and_then(|bytes|serde_json::from_slice::<Value>(&bytes).map_err(|e|e.to_string()))
                .and_then(|value|engine.restore_configuration(&value));
            if let Err(error)=restored{
                if let Ok(bytes)=fs::read(&last){let backup=engine.settings_dir().join(format!("last-session-invalid-{}.json",Utc::now().timestamp_micros()));
                    if let Ok(mut file)=OpenOptions::new().write(true).create_new(true).open(backup){let _=file.write_all(&bytes);}}
                engine.notify(&format!("前回設定を復元できず初期値を使用しました：{error}"),"warning");
            }else{engine.notify("前回の設定を復元しました。受信・Arm・録音は停止中です。","info");}
        }
        Ok(engine)
    }
    fn fs(&self)->f64{num(&self.input,"sample_rate",2_048_000.0)}
    fn center(&self)->f64{num(&self.input,"center_hz",32_256_000.0)}
    fn usable_for(input:&Value)->f64{num(input,"sample_rate",2_048_000.0)*0.9}
    fn usable(&self)->f64{Self::usable_for(&self.input)}
    fn calibration_check_at(&self,now:chrono::DateTime<Utc>)->Result<(),String>{
        let calibration=self.calibration.as_ref().ok_or("校正情報は未読込です")?;
        let band=self.linked_band(&self.measurement)?;
        calibration.check_at(&self.source,self.running,&self.input,&self.source_metadata,self.usable(),band,now)
    }
    fn absolute_offset_at(&self,now:chrono::DateTime<Utc>)->Option<f64>{
        if self.requested_level_unit=="absolute"&&self.calibration_check_at(now).is_ok(){self.calibration.as_ref().map(|cal|cal.offset_db)}else{None}
    }
    fn calibration_status_at(&self,now:chrono::DateTime<Utc>)->Value{
        let check=self.calibration_check_at(now);let valid=check.is_ok();let absolute=valid&&self.requested_level_unit=="absolute";
        json!({"loaded":self.calibration.is_some(),"valid":valid,"unit":if absolute{"dBm"}else{"dBFS"},
            "requested_unit":self.requested_level_unit,"absolute_available":valid,
            "offset_db":self.calibration.as_ref().map(|cal|cal.offset_db),"label":self.calibration.as_ref().map(|cal|cal.label.as_str()),
            "profile":self.calibration.as_ref().map(Calibration::to_json),
            "reason":check.err().unwrap_or_else(||"ユーザー指定の帯域内一律補正が適用可能です。校正精度・不確かさの保証はありません。".into())})
    }
    fn iq_active(&self)->bool{self.iq.as_ref().is_some_and(|r|r.active)}
    fn wav_active(&self)->bool{self.vfos.iter().any(|v|v.wav.as_ref().is_some_and(|r|r.active))}
    fn input_locked(&self)->bool{self.iq_active()||self.trigger.armed}
    fn settings_dir(&self)->PathBuf{self.project.join("data/settings")}
    fn recordings_dir(&self)->PathBuf{self.project.join("data/recordings")}
    fn notify(&mut self,message:&str,level:&str){
        self.notification_id+=1;self.notifications.push_back(json!({"id":self.notification_id,"timestamp":Utc::now().to_rfc3339(),"level":level,"message":message}));
        while self.notifications.len()>50{self.notifications.pop_front();}
    }
    fn in_band(input:&Value,frequency:f64,width:f64)->bool{
        frequency.is_finite()&&width.is_finite()&&width>0.0&&(frequency-num(input,"center_hz",0.0)).abs()+width/2.0<=Self::usable_for(input)/2.0
    }
    fn make_vfo(&self,value:&Value,fallback:u64)->Result<Vfo,String>{
        let id=value.get("id").and_then(identifier).unwrap_or(fallback);
        let frequency=num(value,"frequency_hz",self.center());let bandwidth=num(value,"bandwidth_hz",200_000.0);
        let mode=text(value,"mode","WFM").to_uppercase();let enabled=boolean(value,"enabled",true);
        let format=text(value,"wav_format","PCM16").to_uppercase();
        if !["AM","NFM","WFM"].contains(&mode.as_str()){return Err("復調方式はAM/NFM/WFMです".into());}
        if !["PCM16","PCM24"].contains(&format.as_str()){return Err("WAV形式はPCM16/PCM24です".into());}
        if !frequency.is_finite()||!bandwidth.is_finite()||bandwidth<=0.0||bandwidth>=self.fs(){return Err("VFOの周波数・帯域幅が不正です".into());}
        if enabled&&!Self::in_band(&self.input,frequency,bandwidth){return Err("復調フィルター全幅が有効受信帯域内に入る必要があります".into());}
        let demod=if enabled{Some(VfoDemodulator::try_new(self.fs(),frequency-self.center(),bandwidth,&mode,AUDIO_RATE)?)}else{None};
        Ok(Vfo{id,frequency_hz:frequency,bandwidth_hz:bandwidth,mode,enabled,stopped_reason:value.get("stopped_reason").and_then(Value::as_str).map(str::to_owned),
            wav_format:format,demod,wav:None,audio_level:-300.0,sample_count:0})
    }
    fn vfo_index(&self,id:&Value)->Result<usize,ActionError>{let id=identifier(id).ok_or("VFO IDが不正です")?;self.vfos.iter().position(|v|v.id==id).ok_or_else(||ActionError::bad("VFOが見つかりません"))}
    fn user_marker_index(&self,id:&Value)->Result<usize,ActionError>{
        let id=identifier(id).filter(|id|*id>0).ok_or("ユーザーMarker IDが不正です")?;
        self.measurement["manual_markers"].as_array().and_then(|markers|markers.iter().position(|marker|marker["id"].as_u64()==Some(id)))
            .ok_or_else(||ActionError::new("MARKER_NOT_FOUND","ユーザーMarkerが見つかりません"))
    }
    fn user_marker_frequency(&self,value:&Value)->Result<f64,ActionError>{
        let frequency=value.as_f64().filter(|f|f.is_finite()).ok_or("Marker周波数は有限数で指定してください")?;
        if (frequency-self.center()).abs()>self.usable()/2.0{
            return Err(ActionError::new("OUT_OF_BAND","ユーザーMarkerは有効取得帯域内へ配置してください。中心周波数は自動変更しません。"));
        }Ok(frequency)
    }
    fn user_marker_peaks(&self)->Result<Vec<Value>,ActionError>{
        if self.analyzer.total_valid_frames==0{return Err(ActionError::new("NO_DATA","FFTデータを受信してからピーク検索してください"));}
        let half_span=num(&self.display,"span_hz",self.usable()).min(self.usable())/2.0;
        let low=self.center()-half_span;let high=self.center()+half_span;
        // Ask for every candidate, independently of the automatic marker count
        // and CP/OBW measurement ROI. Candidate selection remains the common
        // original-PSD local-peak definition used by Analyzer.
        let found=self.analyzer.measurements(low,high,self.center(),99.0,self.analyzer.config.fft_size);
        let mut peaks:Vec<Value>=found["markers"].as_array().into_iter().flatten().filter(|marker|{
            marker["frequency_hz"].as_f64().is_some_and(|f|f.is_finite()&&f>=low&&f<=high)
                &&marker["level_dbfs"].as_f64().is_some_and(f64::is_finite)
        }).cloned().collect();
        peaks.sort_by(|a,b|num(a,"frequency_hz",0.0).total_cmp(&num(b,"frequency_hz",0.0)));
        if peaks.is_empty(){return Err(ActionError::new("NO_PEAK","表示Spanと有効取得帯域内に検出可能なピークがありません"));}Ok(peaks)
    }
    fn user_peak_frequency(&self,current:Option<f64>,direction:&str)->Result<f64,ActionError>{
        if !["left","right","max"].contains(&direction){return Err("ピーク検索方向は left/right/max です".into());}
        let peaks=self.user_marker_peaks()?;
        let chosen=if direction=="max"{
            peaks.iter().max_by(|a,b|num(a,"level_dbfs",-300.0).total_cmp(&num(b,"level_dbfs",-300.0)))
        }else{
            let current=current.ok_or("移動元Marker周波数がありません")?;
            let bin=|frequency:f64|((frequency-self.center())/self.analyzer.bin_hz).round() as i64;
            let current_bin=bin(current);
            if direction=="left"{peaks.iter().rev().find(|peak|num(peak,"frequency_hz",current)<current&&bin(num(peak,"frequency_hz",current))!=current_bin)}
            else{peaks.iter().find(|peak|num(peak,"frequency_hz",current)>current&&bin(num(peak,"frequency_hz",current))!=current_bin)}
        };
        chosen.map(|peak|num(peak,"frequency_hz",self.center())).ok_or_else(||ActionError::new("PEAK_BOUNDARY",if direction=="left"{"表示Span内に、これより左のピークはありません"}else{"表示Span内に、これより右のピークはありません"}))
    }
    fn marker_level(&self,frequency:f64)->(Option<f64>,&'static str,&'static str){
        if !frequency.is_finite()||(frequency-self.center()).abs()>self.usable()/2.0{return(None,"out_of_band","現在の有効取得帯域外です。周波数は保持しています。");}
        if self.analyzer.total_valid_frames==0{return(None,"no_data","FFTデータをまだ受信していません。");}
        let index=((frequency-self.center())/self.analyzer.bin_hz+self.analyzer.config.fft_size as f64/2.0).round() as isize;
        if index<0||index>=self.analyzer.last_psd.len()as isize{return(None,"out_of_band","この周波数に対応する解析binがありません。");}
        (Some(db(self.analyzer.last_psd[index as usize]*self.analyzer.enbw_hz)),"ok","")
    }
    fn linked_band(&self,config:&Value)->Result<(f64,f64),String>{
        if let Some(id)=config.get("link_vfo").and_then(identifier){
            let vfo=self.vfos.iter().find(|v|v.id==id).ok_or("連動先VFOがありません")?;
            Ok((vfo.frequency_hz-vfo.bandwidth_hz/2.0,vfo.frequency_hz+vfo.bandwidth_hz/2.0))
        }else{Ok((num(config,"low_hz",self.center()-100_000.0),num(config,"high_hz",self.center()+100_000.0)))}
    }
    // The main metric/threshold fields are the primary condition. UI rows are
    // additional conditions; adding one must not silently ignore the primary.
    fn trigger_controller_config(config:&Value)->Value{
        let mut expanded=config.clone();let mut conditions=vec![json!({"metric":text(config,"metric","peak"),"threshold_db":num(config,"threshold_db",-55.0)})];
        if let Some(extra)=config["conditions"].as_array(){conditions.extend(extra.iter().cloned());}
        expanded["conditions"]=json!(conditions);expanded
    }
    fn band_valid(&self,low:f64,high:f64)->bool{low.is_finite()&&high.is_finite()&&low<high&&low>=self.center()-self.usable()/2.0&&high<=self.center()+self.usable()/2.0}
    fn trigger_band_weights(&self,low:f64,high:f64)->Vec<(usize,f64)>{
        let center=self.center();let half_bin=self.analyzer.bin_hz/2.0;
        self.analyzer.frequency_offsets_hz.iter().enumerate().filter_map(|(index,offset)|{
            let frequency=center+offset;let width=((frequency+half_bin).min(high)-(frequency-half_bin).max(low)).max(0.0);
            if width>0.0{Some((index,width))}else{None}
        }).collect()
    }
    fn trigger_band_powers(psd:&[f64],weights:&[(usize,f64)],enbw_hz:f64)->(f64,f64){
        let(mut peak,mut power)=(0.0f64,0.0);
        for(index,width)in weights{let value=psd[*index];peak=peak.max(value*enbw_hz);power+=value*width;}
        (peak,power)
    }
    fn vfo_conditions(&self,index:usize)->Value{let v=&self.vfos[index];let gain=dsp::demod_audio_scale(&v.mode);
        let normalization=if v.mode=="AM"{"carrier-normalized envelope deviation"}else if v.mode=="WFM"{"phase discriminator divided by 75000 Hz; 50 us de-emphasis"}else{"phase discriminator divided by min(5000 Hz, bandwidth*0.2)"};
        json!({"frequency_hz":v.frequency_hz,"bandwidth_hz":v.bandwidth_hz,"mode":v.mode,"audio_gain":gain,"audio_gain_db":20.0*gain.log10(),"normalization":normalization,
        "input":self.input,"source":self.source,"input_metadata":self.source_metadata,"source_sample":self.last_sample})}
    fn reset_dsp(&mut self)->Result<(),String>{
        let mut config=self.analyzer.config.clone();config.sample_rate=self.fs();
        if text(&self.analysis_extra,"fft_mode","rbw")=="rbw"{config.fft_size=dsp::fft_size_for_rbw(self.fs(),num(&self.analysis_extra,"rbw_hz",750.0),&config.window);}
        dsp::validate_config(&config)?;self.analyzer.reset(Some(config));self.prebuffer=PreTriggerBuffer::new(self.fs(),PRE_BYTES)?;
        self.trigger=TriggerController::new(self.fs(),Self::trigger_controller_config(&self.trigger_config))?;
        let center=self.center();let fs=self.fs();let usable=self.usable();
        for vfo in &mut self.vfos{
            if vfo.enabled&&(vfo.frequency_hz-center).abs()+vfo.bandwidth_hz/2.0>usable/2.0{vfo.enabled=false;vfo.stopped_reason=Some("現在の有効受信帯域外です。中心周波数を戻して手動で再開できます。".into());}
            vfo.demod=if vfo.enabled{Some(VfoDemodulator::try_new(fs,vfo.frequency_hz-center,vfo.bandwidth_hz,&vfo.mode,AUDIO_RATE)?)}else{None};
            vfo.audio_level=-300.0;vfo.sample_count=0;
        }
        self.audio_chunks.clear();self.frozen_markers.clear();self.trigger_last_frame=None;self.had_blocks=false;
        self.last_sample=0;self.overload_streak=0;self.overload_notice=false;
        self.performance=json!({"processing_ms":0.0,"load_percent":0.0,"instant_load_percent":0.0,
            "gap_samples":0,"dropped_samples":0,"valid_frames":0,"input_samples":0});Ok(())
    }
    pub fn on_start(&mut self,source:&str,metadata:Value)->Result<(),String>{
        if self.running{return Err("受信または再生は既に実行中です".into());}
        if let Some(fs)=metadata["sample_rate"].as_f64(){self.input["sample_rate"]=json!(fs);}
        if let Some(center)=metadata["center_hz"].as_f64(){self.input["center_hz"]=json!(center);}
        self.input["gain_actual_db"]=metadata.get("gain_db_actual").cloned().unwrap_or(Value::Null);
        if num(&self.display,"span_hz",self.usable())>self.usable(){self.display["span_hz"]=json!(self.usable());}
        self.source=source.into();self.source_metadata=metadata.clone();self.device=metadata;
        self.device["connected"]=json!(true);self.device["usable_bandwidth_hz"]=json!(self.usable());
        self.device["usable_bandwidth_status"]=json!("provisional_90_percent_of_sample_rate");
        if source=="demo"{self.device["name"]=json!("合成AM/FM信号");self.device["synthetic"]=json!(true);}
        self.reset_dsp()?;self.last_sample=0;
        if self.pending_input_change{
            for index in 0..self.vfos.len(){let conditions=self.vfo_conditions(index);if let Some(writer)=self.vfos[index].wav.as_mut().filter(|r|r.active){
                if let Err(error)=writer.split(conditions){self.notify(&format!("VFOのWAV分割に失敗しました：{error}"),"error");}
            }}
        }
        self.pending_input_change=false;self.running=true;self.overload_streak=0;self.overload_notice=false;Ok(())
    }
    pub fn on_stop(&mut self,reason:&str){
        self.running=false;self.audio_chunks.clear();self.trigger.disarm();
        if let Some(writer)=self.iq.as_mut().filter(|r|r.active){self.iq_last=writer.stop(reason);}
        self.iq_triggered=false;self.iq_start_sample=None;
        if reason=="input_change"{self.pending_input_change=true;}
        else{self.pending_input_change=false;for v in &mut self.vfos{if let Some(writer)=v.wav.as_mut().filter(|r|r.active){writer.stop(reason);}}}
    }
    pub fn reset_for_seek(&mut self,sample:u64)->Result<(),String>{
        if self.input_locked()||self.wav_active(){return Err("IQ/トリガ/WAVを停止してからシークしてください".into());}
        self.reset_dsp()?;self.last_sample=sample;self.performance["input_samples"]=json!(sample);
        self.replay["position_seconds"]=json!(sample as f64/self.fs());Ok(())
    }
    pub fn state(&self)->Value{
        let mut input=self.input.clone();input["usable_bandwidth_hz"]=json!(self.usable());
        let mut analysis=serde_json::to_value(&self.analyzer.config).unwrap_or(json!({}));merge(&mut analysis,&self.analysis_extra);
        analysis["bin_hz"]=json!(self.analyzer.bin_hz);analysis["enbw_hz"]=json!(self.analyzer.enbw_hz);
        analysis["level_unit"]=json!("dBFS");let now=Utc::now();let calibration=self.calibration_status_at(now);
        let mut display=self.display.clone();display["ref_level_dbfs"]=display["ref_level"].clone();
        if let Some(offset)=self.absolute_offset_at(now){display["ref_level"]=json!(num(&self.display,"ref_level",-20.0)+offset);}
        display["unit"]=calibration["unit"].clone();
        let mut trigger=self.trigger_config.clone();merge(&mut trigger,&self.trigger.snapshot());
        // Preserve user terminology in the selected metric; Controller canonicalizes power internally.
        trigger["metric"]=self.trigger_config["metric"].clone();trigger["conditions"]=self.trigger_config["conditions"].clone();trigger["available_pre_seconds"]=json!(self.prebuffer.available_seconds());
        trigger["threshold_unit"]=json!("dBFS");
        trigger["capacity_pre_seconds"]=json!(self.prebuffer.capacity_seconds());trigger["iq_max_seconds"]=json!(IQ_SECONDS);trigger["iq_max_bytes"]=json!(IQ_BYTES);
        let mut measurement=self.measurement.clone();if let Ok((low,high))=self.linked_band(&measurement){measurement["low_hz"]=json!(low);measurement["high_hz"]=json!(high);}
        if let Ok((low,high))=self.linked_band(&trigger){trigger["low_hz"]=json!(low);trigger["high_hz"]=json!(high);}
        let elapsed=self.iq.as_ref().filter(|r|r.active).map(|r|r.duration_seconds());
        json!({"version":"0.1.0","running":self.running,"source":self.source,"input":input,"analysis":analysis,"display":display,
            "measurement":measurement,"trigger":trigger,"selected_vfo_id":self.selected,"vfos":self.vfos.iter().map(|v|v.state(self.selected)).collect::<Vec<_>>(),
            "recording":{"iq_active":self.iq_active(),"iq_triggered":self.iq_triggered,"elapsed_seconds":elapsed,"last_iq":self.iq_last},
            "locks":{"input":self.input_locked(),"center_sample_rate":self.input_locked()||self.wav_active(),"reason":if self.input_locked(){"IQ待機・保存中"}else if self.wav_active(){"WAV録音中は中心周波数・サンプルレートをロック"}else{""}},
            "device":self.device,"performance":self.performance,"replay":self.replay,"layout":self.layout,"notifications":self.notifications,
            "calibration":calibration,
            "implementation":{"engineering_defaults":true,"vfo_limit":"performance-dependent","weak_signal_processing":"optional_not_implemented","d03":"original_subject_pending"}})
    }
    pub fn audio(&self,after:u64)->Value{
        let mut data=vec![];let mut sequence=after.min(self.audio_sequence);
        let oldest=if after==0{self.audio_sequence.saturating_sub(5)}else{after};
        for(seq,chunk)in &self.audio_chunks{if *seq>oldest{data.extend_from_slice(chunk);sequence=*seq;if data.len()>=AUDIO_RATE as usize{break;}}}
        json!({"sequence":sequence,"sample_rate":AUDIO_RATE,"vfo_id":self.selected,"format":"PCM16LE","data_b64":B64.encode(data)})
    }
    pub fn frame(&mut self)->Value{
        let mut frame=self.analyzer.snapshot(num(&self.display,"span_hz",self.usable()),self.center(),768);
        let (low,high)=self.linked_band(&self.measurement).unwrap_or((f64::NAN,f64::NAN));
        let count=self.measurement["marker_count"].as_u64().unwrap_or(6) as usize;
        let mut measurement=if self.band_valid(low,high){self.analyzer.measurements(low,high,self.center(),num(&self.measurement,"obw_percent",99.0),count)}
            else{json!({"status":"unavailable","reason":"測定帯域が有効受信帯域外です","cp_dbfs":null,"cp_avg_dbfs":null,"obw_hz":null,"markers":[]})};
        let mode=text(&self.measurement,"marker_mode","auto");
        if mode=="auto"&&(boolean(&self.measurement,"marker_follow",true)||self.frozen_markers.is_empty()){
            self.frozen_markers=measurement["markers"].as_array().map(|m|m.iter().filter_map(|v|v["frequency_hz"].as_f64()).collect()).unwrap_or_default();
        }
        let manual=self.measurement["manual_markers"].as_array().cloned().unwrap_or_default();
        let mut markers=vec![];
        if mode!="manual"&&self.analyzer.total_valid_frames>0{for(index,frequency)in self.frozen_markers.iter().copied().enumerate(){
            let(level,status,reason)=self.marker_level(frequency);
            markers.push(json!({"rank":markers.len()+1,"kind":"auto","label":format!("A{}",index+1),"selected":false,
                "frequency_hz":frequency,"offset_hz":frequency-self.center(),"level_dbfs":level,"status":status,"reason":reason}));
        }}
        let selected=self.measurement["selected_user_marker_id"].as_u64();
        for marker in manual{
            let Some(frequency)=marker["frequency_hz"].as_f64()else{continue;};let Some(id)=marker["id"].as_u64()else{continue;};
            let(level,status,reason)=self.marker_level(frequency);
            markers.push(json!({"rank":markers.len()+1,"kind":"user","id":id,"label":format!("U{id}"),"selected":selected==Some(id),
                "frequency_hz":frequency,"offset_hz":frequency-self.center(),"level_dbfs":level,"status":status,"reason":reason}));
        }
        let reference=self.measurement["delta_reference"].as_u64().unwrap_or(0) as usize;
        if let Some(base)=markers.get(reference).or(markers.first()).cloned(){for marker in &mut markers{
            marker["delta_hz"]=json!(num(marker,"frequency_hz",0.0)-num(&base,"frequency_hz",0.0));
            marker["delta_db"]=json!(marker["level_dbfs"].as_f64().zip(base["level_dbfs"].as_f64()).map(|(level,reference)|level-reference));
        }}
        measurement["selected_user_marker_id"]=self.measurement["selected_user_marker_id"].clone();
        measurement["markers"]=json!(markers);frame["measurements"]=measurement;frame["sample_index"]=json!(self.last_sample);
        let now=Utc::now();calibration::apply_frame(&mut frame,self.absolute_offset_at(now),self.calibration.as_ref());
        frame["calibration"]=self.calibration_status_at(now);frame
    }
    fn stop_iq(&mut self,reason:&str){if let Some(writer)=self.iq.as_mut(){self.iq_last=writer.stop(reason);}self.iq_triggered=false;self.iq_start_sample=None;}
    fn write_iq_until(&mut self,raw:&[u8],start:u64,end:u64){
        let samples=(raw.len()/2)as u64;let low=start.max(self.iq_start_sample.unwrap_or(start));let high=end.min(start.saturating_add(samples));
        if high<=low{return;}
        let mut failed=None;let mut capped=false;
        if let Some(writer)=self.iq.as_mut().filter(|r|r.active){if let Err(error)=writer.write(&raw[((low-start)*2)as usize..((high-start)*2)as usize],low){failed=Some(error);}capped=writer.limit_reached;}
        if failed.is_some()||capped{
            self.stop_iq(if capped{"recording_limit"}else{"io_error"});self.trigger.disarm();
            self.notify(&failed.map(|e|format!("IQ保存を停止しました：{e}")).unwrap_or_else(||"IQの時間／容量上限に達したため安全終了し、トリガ待機を解除しました。".into()),"warning");
        }
    }
    fn start_iq(&mut self,pre:&[(u64,Vec<u8>)],triggered:bool)->Result<(),String>{
        let mut metadata=self.source_metadata.clone();if !metadata.is_object(){metadata=json!({});}metadata["input_conditions"]=self.input.clone();metadata["source"]=json!(self.source);
        let mut writer=IQRecorder::new(self.recordings_dir(),self.fs(),self.center(),metadata,IQ_BYTES,IQ_SECONDS)?;
        writer.start(pre)?;if writer.limit_reached{return Err("プリトリガが保存上限に達しました".into());}
        self.iq=Some(writer);self.iq_triggered=triggered;self.iq_start_sample=pre.first().map(|chunk|chunk.0);Ok(())
    }
    pub fn process_block(&mut self,start:u64,raw:&[u8])->Result<(),String>{
        if !self.running{return Ok(());}
        if raw.len()%2!=0{return Err("不完全なI/Qサンプルです".into());}if raw.is_empty(){return Ok(());}
        let began=Instant::now();let samples=raw.len()/2;let end=start.saturating_add(samples as u64);
        let gap=if self.had_blocks{start.saturating_sub(self.last_sample)}else{0};
        if self.had_blocks&&start<self.last_sample{return Err("入力サンプルの順序が逆転しました".into());}
        if gap>0{
            self.audio_chunks.clear();for v in &mut self.vfos{if let Some(d)=v.demod.as_mut(){d.reset();}}
            self.notify(&format!("入力欠落を検出：{gap} samples。時間軸とIQメタデータへ記録します。"),"warning");
        }
        self.prebuffer.append(start,raw)?;
        let iq:Vec<Complex32>=raw.chunks_exact(2).map(|v|Complex32::new((v[0]as f32-127.5)/127.5,(v[1]as f32-127.5)/127.5)).collect();
        let result=self.analyzer.process(&iq,start);
        if self.trigger.armed{
            let (low,high)=self.linked_band(&self.trigger_config)?;
            if !self.band_valid(low,high){self.trigger.disarm();self.stop_iq("trigger_band_unavailable");self.notify("監視帯域が有効帯域外になったためトリガを解除しました。","warning");}
            else{
                // Share ROI bin overlaps across every FFT in this input block.
                // Fractional edge widths match Channel Power integration. Peak
                // retains PSD × ENBW, using only bins with positive ROI overlap.
                let weights=self.trigger_band_weights(low,high);
                for(psd,frame_end)in result.fft_frame_psd.iter().zip(result.fft_frame_end_samples.iter()){
                    let step=self.analyzer.hop as u64;
                    let eval_start=self.trigger_last_frame.unwrap_or(frame_end.saturating_sub(step)).max(start.min(*frame_end));
                    let eval_count=frame_end.saturating_sub(eval_start) as usize;self.trigger_last_frame=Some(*frame_end);
                    if eval_count==0{continue;}
                    let(peak,power)=Self::trigger_band_powers(psd,&weights,self.analyzer.enbw_hz);
                    let oldest=end.saturating_sub((self.prebuffer.available_seconds()*self.fs()).round()as u64);
                    let available=eval_start.saturating_sub(oldest)as f64/self.fs();
                    let events=self.trigger.evaluate(&json!({"peak":db(peak),"power":db(power),"integrated":db(power),"available_pre_seconds":available}),eval_start,eval_count)?;
                    for event in events{
                        let at=event["sample_index"].as_u64().unwrap_or(*frame_end);
                        match event["type"].as_str().unwrap_or(""){
                            "trigger"=>{let pre=self.prebuffer.get(num(&self.trigger_config,"pre_seconds",1.0),Some(at))?.0;
                                if let Err(error)=self.start_iq(&pre,true){self.trigger.disarm();self.notify(&format!("トリガIQ保存を開始できません：{error}"),"error");break;}
                                if self.iq_start_sample.is_none(){self.iq_start_sample=Some(at);}
                                if let Some(writer)=self.iq.as_mut(){writer.mark_trigger(at,event.clone());}
                                self.notify("トリガ検出：プリトリガを含むIQ記録を開始しました。","info");},
                            "extend"=>{if let Some(writer)=self.iq.as_mut(){writer.mark_trigger(at,event.clone());}},
                            "complete"=>{self.write_iq_until(raw,start,at);self.stop_iq("post_trigger_complete");
                                let good=self.iq.as_ref().is_some_and(|r|r.last_error.is_none()&&!r.limit_reached);
                                self.trigger.recording_finished(Some(end),good);self.notify(if good{"ポストトリガIQ保存を終了しました。"}else{"IQ保存の異常を記録し、トリガを解除しました。"},if good{"info"}else{"warning"});},_=>{}
                        }
                    }
                    if !self.trigger.armed&&!self.iq_active(){break;}
                }
            }
        }
        if self.iq_active(){let limit=if self.iq_triggered{self.trigger.end_sample.unwrap_or(end).min(end)}else{end};self.write_iq_until(raw,start,limit);}
        let mut audio_selected=None;let mut issues=vec![];
        for v in &mut self.vfos{
            if !v.enabled{continue;}
            let Some(demod)=v.demod.as_mut()else{continue;};let audio=demod.process(&iq);
            v.sample_count+=audio.len()as u64;v.audio_level=db(audio.iter().map(|a|(*a as f64).powi(2)).sum::<f64>()/audio.len().max(1)as f64);
            if let Some(writer)=v.wav.as_mut().filter(|r|r.active){if let Err(error)=writer.write(&audio){issues.push(format!("VFO {}のWAV保存を停止：{error}",v.id));}else if writer.limit_reached{issues.push(format!("VFO {}のWAVがRIFF容量上限に達しました。",v.id));}}
            if v.id==self.selected{audio_selected=Some(audio);}
        }
        for issue in issues{self.notify(&issue,"warning");}
        if let Some(audio)=audio_selected{
            let bytes:Vec<u8>=audio.into_iter().flat_map(|value|{let sample=(value.clamp(-1.0,32767.0/32768.0)*32768.0).round()as i16;sample.to_le_bytes()}).collect();
            if !bytes.is_empty(){self.audio_sequence+=1;self.audio_chunks.push_back((self.audio_sequence,bytes));while self.audio_chunks.len()>16{self.audio_chunks.pop_front();}}
        }
        self.last_sample=end;self.had_blocks=true;if self.source=="replay"{self.replay["position_seconds"]=json!(end as f64/self.fs());}
        let elapsed=began.elapsed().as_secs_f64();let load=elapsed/(samples as f64/self.fs())*100.0;
        let smoothed=num(&self.performance,"load_percent",load)*0.8+load*0.2;
        self.performance=json!({"processing_ms":elapsed*1000.0,"load_percent":smoothed,"instant_load_percent":load,"gap_samples":self.analyzer.gap_samples,
            "dropped_samples":self.analyzer.gap_samples,"valid_frames":self.analyzer.total_valid_frames,"input_samples":end});
        if load>100.0{self.overload_streak+=1;}else{self.overload_streak=0;}
        if self.overload_streak>=3{
            let candidate=self.vfos.iter().enumerate().filter(|(_,v)|v.enabled&&v.id!=self.selected)
                .min_by_key(|(_,v)|(v.wav.as_ref().is_some_and(|w|w.active),v.id)).map(|(i,_)|i);
            if let Some(index)=candidate{let id=self.vfos[index].id;let v=&mut self.vfos[index];if let Some(writer)=v.wav.as_mut().filter(|w|w.active){writer.stop("overload");}
                v.enabled=false;v.demod=None;v.stopped_reason=Some("過負荷のため自動停止。再開は手動です。".into());self.notify(&format!("選択VFOを優先し、VFO {id}を過負荷停止しました。"),"warning");}
            else if !self.overload_notice{self.notify("選択VFO単独でも処理時間が入力時間を超えています。FFT条件や復調帯域を調整してください。","warning");self.overload_notice=true;}
            self.overload_streak=0;
        }
        Ok(())
    }

    // Input planning is side-effect free so Runtime can reject a locked or
    // unconfirmed change without stopping the existing receiver/recorder.
    pub fn plan_input(&self,patch:&Value,confirmed:bool)->Result<Value,ActionError>{
        let object=patch.as_object().ok_or("入力設定が不正です")?;
        let allowed=["center_hz","sample_rate","gain_db","agc","ppm","mode","device_index"];
        if object.keys().any(|k|!allowed.contains(&k.as_str())){return Err("未対応の入力設定項目です".into());}
        let mut candidate=self.input.clone();merge(&mut candidate,patch);Self::validate_input(&candidate)?;
        let changed:Vec<&str>=object.keys().filter(|key|!same(&self.input[key.as_str()],&candidate[key.as_str()])).map(String::as_str).collect();
        if changed.is_empty(){return Ok(candidate);}
        if self.source=="replay"{return Err(ActionError::locked("IQ再生中は元の入力条件を変更できません"));}
        if self.input_locked(){return Err(ActionError::locked("IQのArm開始から保存終了まで入力条件を変更できません"));}
        if self.wav_active()&&changed.iter().any(|k|["center_hz","sample_rate"].contains(k)){return Err(ActionError::locked("WAV録音中は中心周波数とサンプルレートを変更できません"));}
        let impacted:Vec<Value>=self.vfos.iter().filter(|v|v.enabled&&!Self::in_band(&candidate,v.frequency_hz,v.bandwidth_hz)).map(|v|v.config()).collect();
        if !impacted.is_empty()&&!confirmed{return Err(ActionError{message:"この変更で帯域外になるVFOの無効化を確認してください".into(),code:"VFO_IMPACT".into(),impacted_vfos:impacted});}
        Ok(candidate)
    }
    fn validate_input(input:&Value)->Result<(),ActionError>{
        let fs=finite(num(input,"sample_rate",f64::NAN),"サンプルレート")?;
        if ![1_024_000.0,1_536_000.0,2_048_000.0,2_400_000.0].contains(&fs){return Err("この版のサンプルレートは1.024/1.536/2.048/2.4 MS/sです".into());}
        let center=finite(num(input,"center_hz",f64::NAN),"中心周波数")?;
        if !(10_000.0..=1_766_000_000.0).contains(&center){return Err("中心周波数が設定範囲外です".into());}
        if !["auto","tuner","direct_q","direct_i"].contains(&text(input,"mode","auto")){return Err("入力モードが不正です".into());}
        let gain=finite(num(input,"gain_db",f64::NAN),"ゲイン")?;if !(-10.0..=60.0).contains(&gain){return Err("ゲインの指定範囲は-10〜60 dBです。実効値は受信機に合わせて表示します。".into());}
        if input.get("agc").and_then(Value::as_bool).is_none(){return Err("AGCは真偽値で指定してください".into());}
        let ppm=num(input,"ppm",f64::NAN);if !ppm.is_finite()||ppm.fract()!=0.0||ppm.abs()>500.0{return Err("PPMは-500〜500の整数です".into());}
        if input["device_index"].as_u64().is_none(){return Err("受信機番号が不正です".into());}Ok(())
    }
    fn configuration(&self)->Value{let mut input=self.input.clone();if let Some(fields)=input.as_object_mut(){fields.remove("gain_actual_db");}
        json!({"version":1,"input":input,"analysis":{ "config":self.analyzer.config,"extra":self.analysis_extra},
        "display":self.display,"measurement":self.measurement,"trigger":self.trigger_config,"layout":self.layout,
        "calibration":self.calibration.as_ref().map(Calibration::to_json),"requested_level_unit":self.requested_level_unit,
        "vfos":self.vfos.iter().map(Vfo::config).collect::<Vec<_>>(),"selected_vfo_id":self.selected,"preferred_source":if self.source=="demo"{"demo"}else{"live"}})}
    fn save_last(&self)->Result<(),String>{
        fs::create_dir_all(self.settings_dir()).map_err(|e|e.to_string())?;
        let path=self.settings_dir().join("last-session.json");let temp=self.settings_dir().join("last-session.json.tmp");
        let bytes=serde_json::to_vec_pretty(&self.configuration()).map_err(|e|e.to_string())?;
        fs::write(&temp,bytes).and_then(|_|fs::rename(temp,path)).map_err(|e|e.to_string())
    }
    fn restore_configuration(&mut self,value:&Value)->Result<(),String>{
        let mut input=self.input.clone();merge(&mut input,&value["input"]);Self::validate_input(&input).map_err(|e|e.message)?;
        input["gain_actual_db"]=Value::Null;
        let calibration=if value["calibration"].is_null(){None}else{Some(Calibration::from_json(&value["calibration"])?)};
        let unit=text(value,"requested_level_unit","relative");if !["relative","absolute"].contains(&unit){return Err("レベル単位設定が不正です".into());}
        let mut config:Config=serde_json::from_value(value["analysis"]["config"].clone()).map_err(|e|e.to_string())?;config.sample_rate=num(&input,"sample_rate",2_048_000.0);dsp::validate_config(&config)?;
        let mut display=self.display.clone();merge(&mut display,&value["display"]);Self::validate_display(&display,Self::usable_for(&input)).map_err(|e|e.message)?;
        let mut trigger_config=self.trigger_config.clone();merge(&mut trigger_config,&value["trigger"]);
        let trigger=TriggerController::new(config.sample_rate,Self::trigger_controller_config(&trigger_config))?;
        if trigger.pre_seconds>PRE_BYTES as f64/2.0/config.sample_rate{return Err("プリトリガがメモリー容量を超えます".into());}
        let mut measurement=self.measurement.clone();merge(&mut measurement,&value["measurement"]);Self::validate_measurement(&mut measurement).map_err(|e|e.message)?;
        let old_input=self.input.clone();self.input=input;
        let mut vfos=vec![];let settings=value["vfos"].as_array().ok_or("VFO設定がありません")?;
        for(index,setting)in settings.iter().enumerate(){match self.make_vfo(setting,index as u64+1){Ok(v)=>{if vfos.iter().any(|old:&Vfo|old.id==v.id){self.input=old_input;return Err("VFO IDが重複しています".into());}vfos.push(v)},Err(error)=>{self.input=old_input;return Err(error);}}}
        self.vfos=vfos;self.selected=value["selected_vfo_id"].as_u64().filter(|id|self.vfos.iter().any(|v|v.id==*id)).unwrap_or_else(||self.vfos.first().map(|v|v.id).unwrap_or(0));
        self.next_vfo=self.vfos.iter().map(|v|v.id).max().unwrap_or(0)+1;self.analyzer=Analyzer::new(config);self.analysis_extra=value["analysis"]["extra"].clone();
        if !self.analysis_extra.is_object(){self.analysis_extra=json!({"fft_mode":"manual","rbw_hz":self.analyzer.enbw_hz});}
        self.display=display;self.measurement=measurement;self.trigger_config=trigger_config;self.trigger=trigger;
        self.prebuffer=PreTriggerBuffer::new(self.fs(),PRE_BYTES)?;if value["layout"].is_object(){self.layout=value["layout"].clone();}
        self.source=if value["preferred_source"]=="demo"{"demo"}else{"live"}.into();self.calibration=calibration;self.requested_level_unit=unit.into();
        self.source_metadata=json!({});self.audio_chunks.clear();self.frozen_markers.clear();self.had_blocks=false;self.trigger_last_frame=None;self.last_sample=0;
        self.performance=json!({"processing_ms":0.0,"load_percent":0.0,"instant_load_percent":0.0,"gap_samples":0,"dropped_samples":0,"valid_frames":0,"input_samples":0});Ok(())
    }
    fn validate_display(display:&Value,usable:f64)->Result<(),ActionError>{
        let span=finite(num(display,"span_hz",f64::NAN),"スパン")?;if span<100.0||span>usable{return Err(format!("スパンは100 Hz〜{usable:.0} Hzの有効帯域内で指定してください").into());}
        let reference=finite(num(display,"ref_level",f64::NAN),"Reference Level")?;let range=finite(num(display,"range_db",f64::NAN),"表示範囲")?;
        if !(-250.0..=100.0).contains(&reference)||!(1.0..=300.0).contains(&range){return Err("表示レベルの範囲が不正です".into());}Ok(())
    }
    fn validate_measurement(value:&mut Value)->Result<(),ActionError>{
        let low=finite(num(value,"low_hz",f64::NAN),"測定下端")?;let high=finite(num(value,"high_hz",f64::NAN),"測定上端")?;
        if low>=high{return Err("測定下端は上端より低くしてください".into());}
        let obw=finite(num(value,"obw_percent",99.0),"OBW割合")?;if obw<=0.0||obw>=100.0{return Err("OBW割合は0%より大きく100%より小さい値です".into());}
        if value["marker_count"].as_u64().unwrap_or(0)==0{return Err("Marker本数は1以上の整数です".into());}
        if !["auto","manual"].contains(&text(value,"marker_mode","auto")){return Err("Markerモードが不正です".into());}
        let markers=value["manual_markers"].as_array().ok_or("手動Markerの配列が不正です")?.clone();
        let mut used=HashSet::new();
        for marker in &markers{if let Some(raw)=marker.get("id"){
            let id=identifier(raw).filter(|id|*id>0).ok_or("ユーザーMarker IDは1以上の整数です")?;
            if !used.insert(id){return Err("ユーザーMarker IDが重複しています".into());}
        }}
        let mut normalized=Vec::with_capacity(markers.len());let mut next=1u64;
        for marker in markers{
            // Legacy files contained frequency-only objects. Numeric entries
            // are accepted as frequencies as well; all saved output is canonical.
            let frequency=finite(if marker.is_number(){marker.as_f64().unwrap_or(f64::NAN)}else{num(&marker,"frequency_hz",f64::NAN)},"Marker周波数")?;
            let id=if let Some(id)=marker.get("id").and_then(identifier){id}else{
                while used.contains(&next){next=next.checked_add(1).ok_or("ユーザーMarker IDを確保できません")?;}
                let id=next;used.insert(id);id
            };
            normalized.push(json!({"id":id,"frequency_hz":frequency}));
        }
        let selected=if value["selected_user_marker_id"].is_null(){None}else{Some(identifier(&value["selected_user_marker_id"]).filter(|id|*id>0).ok_or("選択ユーザーMarker IDが不正です")?)};
        value["selected_user_marker_id"]=json!(selected.filter(|id|used.contains(id)));
        value["manual_markers"]=json!(normalized);Ok(())
    }
    fn preset_path(&self,name:&str)->Result<PathBuf,ActionError>{
        let name=name.trim();let base=name.split('.').next().unwrap_or("").to_uppercase();
        let reserved=["CON","PRN","AUX","NUL"].contains(&base.as_str())||((base.starts_with("COM")||base.starts_with("LPT"))&&base.len()==4&&base.as_bytes()[3].is_ascii_digit());
        if name.is_empty()||name.chars().count()>80||name.ends_with('.')||name.chars().any(|c|c.is_control()||['<','>',':','"','/','\\','|','?','*'].contains(&c))||reserved{return Err("設定名は80文字以内で、パス記号や予約名を使わず指定してください".into());}
        Ok(self.settings_dir().join("presets").join(format!("{name}.json")))
    }
    pub fn files(&self)->Value{
        let mut files=vec![];
        if let Ok(entries)=fs::read_dir(self.recordings_dir()){for entry in entries.flatten(){let path=entry.path();let ext=path.extension().and_then(|x|x.to_str()).unwrap_or("");
            if !["sigmf-meta","wav"].contains(&ext){continue;}if let Ok(meta)=entry.metadata(){files.push(json!({"name":entry.file_name().to_string_lossy(),"path":path.to_string_lossy(),"size":meta.len(),"kind":if ext=="sigmf-meta"{"iq"}else{"wav"}}));}}}
        files.sort_by(|a,b|b["name"].as_str().cmp(&a["name"].as_str()));
        let mut presets=vec![];if let Ok(entries)=fs::read_dir(self.settings_dir().join("presets")){for entry in entries.flatten(){if entry.path().extension().and_then(|e|e.to_str())==Some("json"){presets.push(json!({"name":entry.path().file_stem().unwrap().to_string_lossy()}));}}}
        json!({"files":files,"presets":presets})
    }
    pub fn apply(&mut self,body:&Value)->Result<Value,ActionError>{
        self.apply_action(body)?;
        if text(body,"action","")!="trace_reset"{
            if let Err(error)=self.save_last(){self.notify(&format!("設定の保存に失敗しました：{error}"),"warning");}
        }
        Ok(self.state())
    }
    fn apply_action(&mut self,body:&Value)->Result<(),ActionError>{
        match text(body,"action",""){
            "trace_reset"=>self.analyzer.reset_traces(),
            "set_input"=>{
                let patch=&body["input"];let candidate=self.plan_input(patch,boolean(body,"confirm_impacted",false))?;
                let changed=patch.as_object().unwrap().keys().any(|key|!same(&self.input[key.as_str()],&candidate[key.as_str()]));
                if !changed{return Ok(());}
                for v in &mut self.vfos{if v.enabled&&!Self::in_band(&candidate,v.frequency_hz,v.bandwidth_hz){
                    if let Some(writer)=v.wav.as_mut().filter(|w|w.active){writer.stop("input_band_changed");}
                    v.enabled=false;v.demod=None;v.stopped_reason=Some("中心周波数の変更を承認したため無効化。再開は手動です。".into());
                }}
                self.input=candidate;self.input["gain_actual_db"]=Value::Null;
                let usable=self.usable();if num(&self.display,"span_hz",usable)>usable{self.display["span_hz"]=json!(usable);}
                self.reset_dsp()?;self.notify("入力条件を変更し、密度・解析履歴をクリアしました。","info");
            },
            "set_display"=>{
                let patch=&body["display"];if !patch.is_object(){return Err("表示設定が不正です".into());}
                let mut candidate=self.display.clone();merge(&mut candidate,patch);
                if patch.get("ref_level").is_some(){
                    let explicit=patch.get("ref_level_unit").and_then(Value::as_str);
                    let offset=match explicit{
                        Some("dBFS")=>0.0,
                        // The source may have changed since the UI captured this
                        // value. A saved offset can convert the old display value
                        // without making the now-invalid calibration applicable.
                        Some("dBm")=>self.calibration.as_ref().map(|cal|cal.offset_db).ok_or("dBmの表示値を換算する校正情報がありません")?,
                        Some(_)=>return Err("Reference Levelの単位はdBFS/dBmです".into()),
                        None=>self.absolute_offset_at(Utc::now()).unwrap_or(0.0),
                    };
                    candidate["ref_level"]=json!(finite(num(patch,"ref_level",f64::NAN),"Reference Level")?-offset);
                }
                if let Some(object)=candidate.as_object_mut(){object.remove("ref_level_unit");}
                Self::validate_display(&candidate,self.usable())?;
                if let Some(traces)=candidate["traces"].as_object(){for key in ["live","max","min","avg"]{if traces.get(key).and_then(Value::as_bool).is_none(){return Err("TraceのON/OFF設定が不正です".into());}}}else{return Err("Trace設定が不正です".into());}
                let mut config=self.analyzer.config.clone();config.ref_level=num(&candidate,"ref_level",-20.0);config.range_db=num(&candidate,"range_db",100.0);
                self.analyzer.update_config(config)?;self.display=candidate;
            },
            "set_analysis"=>{
                let patch=&body["analysis"];let object=patch.as_object().ok_or("解析設定が不正です")?;
                // Density bounds are stored as dBFS. The UI captures its edit
                // unit/offset and converts before submitting, so a calibration
                // change during editing cannot silently reinterpret a dBm value.
                if let Some(unit)=object.get("density_level_unit"){
                    if unit.as_str()!=Some("dBFS"){return Err("Densityの上下限は編集時の校正値でdBFSに換算して送信してください".into());}
                }
                if let Some(fs)=object.get("sample_rate"){if fs.as_f64()!=Some(self.fs()){return Err("サンプルレートは入力設定から変更してください".into());}}
                let mut value=serde_json::to_value(&self.analyzer.config).map_err(|e|e.to_string())?;merge(&mut value,patch);value["sample_rate"]=json!(self.fs());
                if let Some(fields)=value.as_object_mut(){fields.remove("density_level_unit");}
                let mut config:Config=serde_json::from_value(value).map_err(|e|e.to_string())?;
                let mut extra=self.analysis_extra.clone();for key in ["fft_mode","rbw_hz"]{if let Some(v)=object.get(key){extra[key]=v.clone();}}
                if let Some(rbw)=object.get("target_rbw_hz"){extra["rbw_hz"]=rbw.clone();}
                if !["manual","rbw"].contains(&text(&extra,"fft_mode","rbw")){return Err("FFT操作方式が不正です".into());}
                let rbw=finite(num(&extra,"rbw_hz",750.0),"RBW")?;if rbw<=0.0{return Err("RBWは正の値が必要です".into());}
                if text(&extra,"fft_mode","rbw")=="rbw"{config.fft_size=dsp::fft_size_for_rbw(self.fs(),rbw,&config.window);}
                if config.max_density_history_mb>256.0{return Err("この版の密度履歴メモリー上限は256 MiBです".into());}
                dsp::validate_config(&config)?;
                let old=&self.analyzer.config;let fft_changed=old.fft_size!=config.fft_size||old.window!=config.window||old.overlap!=config.overlap||old.sample_rate!=config.sample_rate;
                if fft_changed&&self.trigger.armed{return Err(ActionError::locked("トリガ待機中は判定に使うFFT条件を変更できません。先に待機解除してください。"));}
                let outcome=self.analyzer.update_config(config)?;self.analysis_extra=extra;
                self.performance["valid_frames"]=json!(self.analyzer.total_valid_frames);
                self.performance["gap_samples"]=json!(self.analyzer.gap_samples);self.performance["dropped_samples"]=json!(self.analyzer.gap_samples);
                self.display["ref_level"]=json!(self.analyzer.config.ref_level);self.display["range_db"]=json!(self.analyzer.config.range_db);
                if boolean(&outcome,"history_cleared",false){
                    if outcome["history_scope"]=="analysis"{self.frozen_markers.clear();}
                    self.notify(if outcome["reason"]=="density_grid_changed"{"Densityの縦軸を変更したため密度履歴だけをクリアしました。"}
                        else{"解析条件を変更したため密度履歴をクリアしました。"},"info");
                }
            },
            "set_measurement"=>{
                if !body["measurement"].is_object(){return Err("測定設定が不正です".into());}
                let mut candidate=self.measurement.clone();merge(&mut candidate,&body["measurement"]);Self::validate_measurement(&mut candidate)?;
                if let Some(id)=candidate.get("link_vfo").and_then(identifier){if !self.vfos.iter().any(|v|v.id==id){return Err("連動先VFOがありません".into());}}
                self.measurement=candidate;self.frozen_markers.clear();
            },
            "peak_search"=>{self.frozen_markers.clear();},
            "user_marker_add"=>{
                let frequency=if let Some(value)=body.get("frequency_hz"){self.user_marker_frequency(value)?}else{self.user_peak_frequency(None,"max")?};
                let markers=self.measurement["manual_markers"].as_array_mut().ok_or("ユーザーMarker設定が不正です")?;
                let id=markers.iter().filter_map(|marker|marker["id"].as_u64()).max().unwrap_or(0).checked_add(1).ok_or("ユーザーMarker IDを確保できません")?;
                markers.push(json!({"id":id,"frequency_hz":frequency}));self.measurement["selected_user_marker_id"]=json!(id);
            },
            "user_marker_select"=>{
                let id=body.get("id").ok_or("選択するユーザーMarker IDかnullを指定してください")?;
                if id.is_null(){self.measurement["selected_user_marker_id"]=Value::Null;}
                else{let index=self.user_marker_index(id)?;self.measurement["selected_user_marker_id"]=self.measurement["manual_markers"][index]["id"].clone();}
            },
            "user_marker_move"=>{
                let index=self.user_marker_index(&body["id"])?;let frequency=self.user_marker_frequency(&body["frequency_hz"])?;
                self.measurement["manual_markers"][index]["frequency_hz"]=json!(frequency);
            },
            "user_marker_peak"=>{
                let index=self.user_marker_index(&body["id"])?;let current=self.measurement["manual_markers"][index]["frequency_hz"].as_f64().ok_or("Marker周波数が不正です")?;
                let frequency=self.user_peak_frequency(Some(current),text(body,"direction",""))?;
                self.measurement["manual_markers"][index]["frequency_hz"]=json!(frequency);
            },
            "user_marker_delete"=>{
                let index=self.user_marker_index(&body["id"])?;
                let removed=self.measurement["manual_markers"].as_array_mut().unwrap().remove(index);
                if self.measurement["selected_user_marker_id"]==removed["id"]{self.measurement["selected_user_marker_id"]=Value::Null;}
            },
            "calibration_load"=>{
                self.calibration=Some(Calibration::load_at(&body["calibration"],Utc::now())?);
                self.notify("ユーザー指定の校正情報を読み込みました。実効入力・期限・周波数範囲が一致した場合だけ絶対単位を選べます。","info");
            },
            "calibration_clear"=>{self.calibration=None;self.requested_level_unit="relative".into();self.notify("校正情報を解除し、相対単位へ戻しました。","info");},
            "set_level_unit"=>{
                let unit=text(body,"unit","");if !["relative","absolute"].contains(&unit){return Err("レベル単位は relative/absolute です".into());}
                if unit=="absolute"{self.calibration_check_at(Utc::now()).map_err(|reason|ActionError::new("CALIBRATION_INVALID",reason))?;}
                self.requested_level_unit=unit.into();
            },
            "set_trigger"=>{
                if self.trigger.armed{return Err(ActionError::locked("トリガ待機を解除してから条件を変更してください"));}
                if !body["trigger"].is_object(){return Err("トリガ設定が不正です".into());}
                let mut config=self.trigger_config.clone();merge(&mut config,&body["trigger"]);
                let low=finite(num(&config,"low_hz",f64::NAN),"監視下端")?;let high=finite(num(&config,"high_hz",f64::NAN),"監視上端")?;
                if low>=high{return Err("監視下端は上端より低くしてください".into());}
                if let Some(id)=config.get("link_vfo").and_then(identifier){if !self.vfos.iter().any(|v|v.id==id){return Err("連動先VFOがありません".into());}}
                let controller=TriggerController::new(self.fs(),Self::trigger_controller_config(&config))?;
                if controller.pre_seconds>self.prebuffer.capacity_seconds(){return Err(format!("プリトリガは確保可能時間 {:.3} s 以内にしてください",self.prebuffer.capacity_seconds()).into());}
                if controller.pre_seconds+controller.post_seconds>IQ_SECONDS{return Err(format!("初期の前後収録時間は合計 {IQ_SECONDS} s 以内です。再検出延長時も上限で安全終了します。").into());}
                self.trigger_config=config;self.trigger=controller;self.trigger_last_frame=None;
            },
            "arm"=>{
                if !self.running{return Err("受信・再生を開始してからArmしてください".into());}
                if self.iq_active(){return Err(ActionError::locked("実行中のIQ保存を終了してからArmしてください"));}
                let(low,high)=self.linked_band(&self.trigger_config)?;if !self.band_valid(low,high){return Err("トリガ監視帯域が有効受信帯域外です".into());}
                self.trigger.arm(Some(self.last_sample),self.prebuffer.available_seconds())?;self.trigger_last_frame=None;
                self.notify("トリガをArmしました。入力条件をロックしています。","info");
            },
            "disarm"=>{self.trigger.disarm();if self.iq_triggered{self.stop_iq("manual_disarm");}self.trigger_last_frame=None;},
            "iq_start"=>{
                if !self.running{return Err("受信・再生を開始してからIQ保存してください".into());}
                if self.trigger.armed{return Err(ActionError::locked("トリガ待機中は手動IQ保存を開始できません"));}
                if !self.iq_active(){self.start_iq(&[],false)?;self.notify("手動IQ保存を開始しました。入力条件をロックしています。","info");}
            },
            "iq_stop"=>{if self.iq_triggered&&self.trigger.armed{return Err(ActionError::locked("トリガIQは待機解除で終了してください"));}self.stop_iq("manual");},
            "add_vfo"=>{
                let mut config=if body["vfo"].is_object(){body["vfo"].clone()}else{json!({})};config["id"]=json!(self.next_vfo);
                let vfo=self.make_vfo(&config,self.next_vfo)?;self.next_vfo+=1;if self.vfos.is_empty(){self.selected=vfo.id;}self.vfos.push(vfo);
            },
            "select_vfo"=>{let index=self.vfo_index(&body["id"])?;self.selected=self.vfos[index].id;self.audio_chunks.clear();},
            "update_vfo"=>{
                let index=self.vfo_index(&body["id"])?;let patch=body["patch"].as_object().ok_or("VFO設定が不正です")?;
                if patch.keys().any(|k|!["frequency_hz","bandwidth_hz","mode","wav_format","enabled"].contains(&k.as_str())){return Err("未対応のVFO設定項目です".into());}
                let original=self.vfos[index].config();let mut config=original.clone();merge(&mut config,&body["patch"]);
                if patch.keys().all(|key|same(&original[key.as_str()],&config[key.as_str()])){return Ok(());}
                let conditions_changed=["frequency_hz","bandwidth_hz","mode"].iter().any(|k|!same(&original[*k],&config[*k]));
                if conditions_changed&&self.trigger.armed&&self.trigger_config["link_vfo"].as_u64()==Some(self.vfos[index].id){return Err(ActionError::locked("トリガが連動中のVFO条件は待機解除後に変更してください"));}
                let mut replacement=self.make_vfo(&config,self.vfos[index].id)?;
                if !conditions_changed&&original["enabled"]==config["enabled"]{replacement.demod=self.vfos[index].demod.take();replacement.audio_level=self.vfos[index].audio_level;replacement.sample_count=self.vfos[index].sample_count;}
                let split=conditions_changed||original["wav_format"]!=config["wav_format"];
                let old_wav=self.vfos[index].wav.take();replacement.wav=old_wav;
                self.vfos[index]=replacement;
                if split&&self.vfos[index].wav.as_ref().is_some_and(|w|w.active){
                    if let Some(writer)=self.vfos[index].wav.as_mut(){writer.stop("conditions_changed");}
                    let mut writer=WavRecorder::new(self.recordings_dir(),&self.vfos[index].id.to_string(),AUDIO_RATE,&self.vfos[index].wav_format)?;
                    match writer.start(self.vfo_conditions(index)){Ok(_)=>self.vfos[index].wav=Some(writer),Err(error)=>self.notify(&format!("VFO条件は変更しましたがWAV再開に失敗：{error}"),"error")}
                }
                if self.vfos[index].id==self.selected&&(conditions_changed||original["enabled"]!=config["enabled"]){self.audio_chunks.clear();}
                if !self.vfos[index].enabled{if let Some(writer)=self.vfos[index].wav.as_mut().filter(|w|w.active){writer.stop("vfo_disabled");}}
            },
            "delete_vfo"=>{
                let index=self.vfo_index(&body["id"])?;let id=self.vfos[index].id;
                if self.trigger.armed&&self.trigger_config["link_vfo"].as_u64()==Some(id){return Err(ActionError::locked("トリガ連動中のVFOは待機解除してから削除してください"));}
                if let Some(writer)=self.vfos[index].wav.as_mut().filter(|w|w.active){writer.stop("vfo_deleted");}self.vfos.remove(index);
                if self.selected==id{self.selected=self.vfos.first().map(|v|v.id).unwrap_or(0);self.audio_chunks.clear();}
                if self.measurement["link_vfo"].as_u64()==Some(id){self.measurement["link_vfo"]=Value::Null;}
                if self.trigger_config["link_vfo"].as_u64()==Some(id){self.trigger_config["link_vfo"]=Value::Null;}
            },
            "resume_vfo"=>{
                let index=self.vfo_index(&body["id"])?;if self.vfos[index].enabled{return Ok(());}let mut config=self.vfos[index].config();config["enabled"]=json!(true);config["stopped_reason"]=Value::Null;
                let mut replacement=self.make_vfo(&config,self.vfos[index].id)?;replacement.wav=self.vfos[index].wav.take();self.vfos[index]=replacement;
                self.notify("VFOを手動で再開しました。停止したWAV録音は自動再開しません。","info");
            },
            "stop_vfo"=>{
                let index=self.vfo_index(&body["id"])?;if let Some(writer)=self.vfos[index].wav.as_mut().filter(|w|w.active){writer.stop("vfo_stopped");}
                self.vfos[index].enabled=false;self.vfos[index].demod=None;self.vfos[index].stopped_reason=Some("手動停止".into());if self.vfos[index].id==self.selected{self.audio_chunks.clear();}
            },
            "wav_start"=>{
                if !self.running{return Err("受信・再生を開始してからWAV録音してください".into());}let index=self.vfo_index(&body["id"])?;
                if !self.vfos[index].enabled{return Err("停止中VFOは録音できません。先に手動で再開してください。".into());}
                if !self.vfos[index].wav.as_ref().is_some_and(|w|w.active){let mut writer=WavRecorder::new(self.recordings_dir(),&self.vfos[index].id.to_string(),AUDIO_RATE,&self.vfos[index].wav_format)?;
                    writer.start(self.vfo_conditions(index))?;self.vfos[index].wav=Some(writer);}
            },
            "wav_stop"=>{let index=self.vfo_index(&body["id"])?;if let Some(writer)=self.vfos[index].wav.as_mut(){writer.stop("manual");}},
            "set_layout"=>{
                let mut layout=self.layout.clone();if !body["layout"].is_object(){return Err("画面配置が不正です".into());}merge(&mut layout,&body["layout"]);
                layout["spectrum_ratio"]=json!(finite(num(&layout,"spectrum_ratio",0.6),"Spectrum比率")?.clamp(0.52,0.75));
                layout["vfo_width"]=json!(finite(num(&layout,"vfo_width",340.0),"VFO幅")?.clamp(280.0,460.0));
                layout["bottom_ratio"]=json!(finite(num(&layout,"bottom_ratio",0.5),"下段比率")?.clamp(0.3,0.7));self.layout=layout;
            },
            "preset_save"=>{
                let path=self.preset_path(text(body,"name",""))?;fs::create_dir_all(path.parent().unwrap()).map_err(|e|e.to_string())?;
                let bytes=serde_json::to_vec_pretty(&self.configuration()).map_err(|e|e.to_string())?;
                let mut file=OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e|format!("設定を保存できません（同名の場合は別名にしてください）：{e}"))?;
                file.write_all(&bytes).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?;
            },
            "preset_load"=>{
                if self.running||self.input_locked()||self.wav_active()||self.source=="replay"{return Err(ActionError::locked("受信・再生・記録を停止し、IQ再生を閉じてから設定を読み込んでください"));}
                let path=self.preset_path(text(body,"name",""))?;let bytes=fs::read(path).map_err(|e|e.to_string())?;
                let config:Value=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;self.restore_configuration(&config)?;
                self.notify("設定を読み込みました。受信・Arm・録音は手動で開始してください。","info");
            },
            _=>return Err(ActionError::new("unknown_action","未対応の操作です")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    fn engine()->Engine{Engine::new(std::env::temp_dir().join(format!("spectrum-engine-test-{}-{}",std::process::id(),Utc::now().timestamp_nanos_opt().unwrap()))).unwrap()}
    fn trace_test_iq(samples:usize,amplitude:f64)->Vec<u8>{
        (0..samples).flat_map(|k|{let phase=-std::f64::consts::TAU*256000.0*k as f64/2048000.0;
            [(127.5+amplitude*phase.cos()).round()as u8,(127.5+amplitude*phase.sin()).round()as u8]}).collect()
    }
    fn assert_trace_seeded_from_live(frame:&Value){
        for name in ["max_db","min_db","avg_db"]{assert_eq!(frame["spectrum"][name],frame["spectrum"]["live_db"],"{name}");}
    }
    #[test]
    fn trace_reset_is_idempotent_when_empty_and_stopped_without_saving_settings(){
        let mut e=engine();let initial=e.state();assert!(!e.project.exists());
        e.apply(&json!({"action":"trace_reset"})).unwrap();e.apply(&json!({"action":"trace_reset"})).unwrap();
        assert_eq!(e.state(),initial);assert!(!e.project.exists());
        let empty=e.frame();for key in ["live_db","max_db","min_db","avg_db"]{
            assert!(empty["spectrum"][key].as_array().unwrap().iter().all(|v|v.as_f64().is_some_and(|level|level.is_finite()&&level<=-290.0)));
        }
        e.on_start("demo",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();
        e.process_block(0,&trace_test_iq(16384,20.0)).unwrap();e.on_stop("test");let state=e.state();
        e.apply(&json!({"action":"trace_reset"})).unwrap();assert_eq!(e.state(),state);assert_trace_seeded_from_live(&e.frame());
    }
    #[test]
    fn trace_reset_preserves_active_iq_wav_prebuffer_markers_and_stream(){
        let mut e=engine();e.on_start("demo",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();
        e.apply(&json!({"action":"wav_start","id":1})).unwrap();e.apply(&json!({"action":"iq_start"})).unwrap();
        e.process_block(0,&trace_test_iq(16384,40.0)).unwrap();e.process_block(16384,&trace_test_iq(16384,8.0)).unwrap();
        e.apply(&json!({"action":"user_marker_add","frequency_hz":32240000.0})).unwrap();
        let before_frame=e.frame();let before=e.state();let before_config=e.configuration();
        let pre=e.prebuffer.get(1.0,None).unwrap();let audio=e.audio(0);let last=e.last_sample;
        let iq=e.iq.as_ref().unwrap().summary();let iq_samples=e.iq.as_ref().unwrap().samples_written;
        let wav=e.vfos[0].wav.as_ref().unwrap();let wav_samples=wav.samples_written;
        let wav_state=json!({"active":wav.active,"path":wav.path,"meta_path":wav.meta_path,"samples":wav.samples_written,"bytes":wav.bytes_written,"clips":wav.clipped_samples,"segments":wav.segments});
        let files=[e.iq.as_ref().unwrap().data_path.clone().unwrap(),e.iq.as_ref().unwrap().meta_path.clone().unwrap(),wav.path.clone().unwrap(),wav.meta_path.clone().unwrap(),e.settings_dir().join("last-session.json")];
        let bytes:Vec<Vec<u8>>=files.iter().map(|p|fs::read(p).unwrap()).collect();
        let old_peak=before_frame["spectrum"]["max_db"].as_array().unwrap().iter().filter_map(Value::as_f64).fold(-300.0,f64::max);
        e.apply(&json!({"action":"trace_reset"})).unwrap();e.apply(&json!({"action":"trace_reset"})).unwrap();
        assert_eq!(e.state(),before);assert_eq!(e.configuration(),before_config);assert_eq!(e.prebuffer.get(1.0,None).unwrap(),pre);assert_eq!(e.audio(0),audio);assert_eq!(e.last_sample,last);
        assert_eq!(e.iq.as_ref().unwrap().summary(),iq);let wav=e.vfos[0].wav.as_ref().unwrap();
        assert_eq!(json!({"active":wav.active,"path":wav.path,"meta_path":wav.meta_path,"samples":wav.samples_written,"bytes":wav.bytes_written,"clips":wav.clipped_samples,"segments":wav.segments}),wav_state);
        assert_eq!(files.iter().map(|p|fs::read(p).unwrap()).collect::<Vec<_>>(),bytes);
        let after=e.frame();assert_trace_seeded_from_live(&after);
        for key in ["density","waterfall","valid_frames","invalid_frames","gap_samples","sample_index"]{assert_eq!(after[key],before_frame[key],"{key}");}
        for key in ["cp_dbfs","obw_hz","obw_low_hz","obw_high_hz","markers"]{assert_eq!(after["measurements"][key],before_frame["measurements"][key],"{key}");}
        assert_eq!(after["measurements"]["cp_avg_dbfs"],after["measurements"]["cp_dbfs"]);
        let reset_peak=after["spectrum"]["max_db"].as_array().unwrap().iter().filter_map(Value::as_f64).fold(-300.0,f64::max);assert!(reset_peak<old_peak-10.0);
        e.process_block(last,&trace_test_iq(16384,4.0)).unwrap();assert!(e.running&&e.iq_active()&&e.wav_active());
        assert_eq!(e.iq.as_ref().unwrap().samples_written,iq_samples+16384);assert!(e.vfos[0].wav.as_ref().unwrap().samples_written>wav_samples);
        let continued=e.frame();let continued_peak=continued["spectrum"]["max_db"].as_array().unwrap().iter().filter_map(Value::as_f64).fold(-300.0,f64::max);assert!(continued_peak<old_peak-10.0);
        e.on_stop("test");
    }
    #[test]
    fn trace_reset_during_replay_preserves_armed_trigger_and_sample_timing(){
        let mut e=engine();e.on_start("replay",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();
        e.apply(&json!({"action":"set_trigger","trigger":{"pre_seconds":0.001,"threshold_db":10.0}})).unwrap();
        e.process_block(0,&trace_test_iq(16384,20.0)).unwrap();e.apply(&json!({"action":"arm"})).unwrap();
        e.process_block(16384,&trace_test_iq(16384,8.0)).unwrap();assert!(e.trigger.armed);assert!(e.trigger_last_frame.is_some());
        let before=e.state();let trigger=e.trigger.snapshot();let trigger_frame=e.trigger_last_frame;let pre=e.prebuffer.get(1.0,None).unwrap();
        let previous=e.frame();e.apply(&json!({"action":"trace_reset"})).unwrap();
        assert_eq!(e.state(),before);assert_eq!(e.trigger.snapshot(),trigger);assert_eq!(e.trigger_last_frame,trigger_frame);assert_eq!(e.prebuffer.get(1.0,None).unwrap(),pre);
        let after=e.frame();assert_trace_seeded_from_live(&after);assert_eq!(after["density"],previous["density"]);assert_eq!(after["waterfall"],previous["waterfall"]);assert_eq!(after["sample_index"],previous["sample_index"]);
        e.on_stop("test");
    }
    #[test]
    fn density_axis_edit_preserves_recordings_frozen_markers_traces_and_stream(){
        let mut e=engine();e.on_start("demo",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();
        e.apply(&json!({"action":"wav_start","id":1})).unwrap();e.apply(&json!({"action":"iq_start"})).unwrap();
        e.process_block(0,&trace_test_iq(16384,40.0)).unwrap();e.process_block(16384,&trace_test_iq(16384,8.0)).unwrap();
        e.apply(&json!({"action":"user_marker_add","frequency_hz":32000000.0})).unwrap();
        e.apply(&json!({"action":"set_measurement","measurement":{"marker_follow":false}})).unwrap();
        let before_frame=e.frame();assert!(!e.frozen_markers.is_empty());let frozen=e.frozen_markers.clone();let before=e.state();
        let pre=e.prebuffer.get(1.0,None).unwrap();let audio=e.audio(0);let last=e.last_sample;
        let iq=e.iq.as_ref().unwrap().summary();let iq_samples=e.iq.as_ref().unwrap().samples_written;
        let wav=e.vfos[0].wav.as_ref().unwrap();let wav_samples=wav.samples_written;
        let files=[e.iq.as_ref().unwrap().data_path.clone().unwrap(),e.iq.as_ref().unwrap().meta_path.clone().unwrap(),wav.path.clone().unwrap(),wav.meta_path.clone().unwrap()];
        let bytes:Vec<Vec<u8>>=files.iter().map(|p|fs::read(p).unwrap()).collect();
        e.apply(&json!({"action":"set_analysis","analysis":{"density_min_db":-100.0,"density_max_db":-10.0,"density_levels":64,"density_level_unit":"dBFS"}})).unwrap();
        let after=e.state();for key in ["running","source","input","display","measurement","trigger","selected_vfo_id","vfos","recording","locks","device","performance","replay","layout"]{assert_eq!(after[key],before[key],"{key}");}
        assert_eq!(e.prebuffer.get(1.0,None).unwrap(),pre);assert_eq!(e.audio(0),audio);assert_eq!(e.last_sample,last);
        assert_eq!(e.iq.as_ref().unwrap().summary(),iq);assert_eq!(e.frozen_markers,frozen);assert_eq!(files.iter().map(|p|fs::read(p).unwrap()).collect::<Vec<_>>(),bytes);
        let after_frame=e.frame();for key in ["spectrum","waterfall","measurements","valid_frames","invalid_frames","gap_samples","sample_index"]{assert_eq!(after_frame[key],before_frame[key],"{key}");}
        assert_eq!(after_frame["density"]["valid_frames"],0);assert_eq!(after_frame["density"]["height"],64);
        assert_eq!(after_frame["density"]["min_db"],-100.0);assert_eq!(after_frame["density"]["max_db"],-10.0);
        assert!(B64.decode(after_frame["density"]["data_b64"].as_str().unwrap()).unwrap().iter().all(|v|*v==0));
        e.process_block(last,&trace_test_iq(16384,8.0)).unwrap();assert!(e.running&&e.iq_active()&&e.wav_active());
        assert_eq!(e.iq.as_ref().unwrap().samples_written,iq_samples+16384);assert!(e.vfos[0].wav.as_ref().unwrap().samples_written>wav_samples);
        let global=e.analyzer.total_valid_frames;assert!(global>before["performance"]["valid_frames"].as_u64().unwrap());
        assert_eq!(e.performance["valid_frames"],global);assert!(e.analyzer.density_valid_frames>0&&e.analyzer.density_valid_frames<global);
        let current=e.frame();let notices=e.notifications.clone();e.apply(&json!({"action":"set_analysis","analysis":{"density_min_db":-100.0,"density_max_db":-10.0,"density_level_unit":"dBFS"}})).unwrap();
        assert_eq!(e.frame(),current);assert_eq!(e.notifications,notices);assert_eq!(e.frozen_markers,frozen);e.on_stop("test");
    }
    #[test]
    fn density_axis_edit_during_replay_keeps_armed_trigger_and_prebuffer(){
        let mut e=engine();e.on_start("replay",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();
        e.apply(&json!({"action":"set_trigger","trigger":{"pre_seconds":0.001,"threshold_db":10.0}})).unwrap();
        e.process_block(0,&trace_test_iq(16384,20.0)).unwrap();e.apply(&json!({"action":"arm"})).unwrap();
        e.process_block(16384,&trace_test_iq(16384,8.0)).unwrap();let before=e.frame();let performance=e.performance.clone();
        let trigger=e.trigger.snapshot();let trigger_frame=e.trigger_last_frame;let pre=e.prebuffer.get(1.0,None).unwrap();let last=e.last_sample;
        e.apply(&json!({"action":"set_analysis","analysis":{"density_min_db":-90.0,"density_max_db":-20.0}})).unwrap();
        assert_eq!(e.trigger.snapshot(),trigger);assert_eq!(e.trigger_last_frame,trigger_frame);assert_eq!(e.prebuffer.get(1.0,None).unwrap(),pre);
        assert_eq!(e.performance,performance);assert_eq!(e.last_sample,last);assert!(e.running&&e.trigger.armed);assert_eq!(e.source,"replay");
        let after=e.frame();for key in ["spectrum","waterfall","measurements","valid_frames","sample_index"]{assert_eq!(after[key],before[key],"{key}");}
        assert_eq!(after["density"]["valid_frames"],0);e.on_stop("test");
    }
    #[test]
    fn density_axis_invalid_inputs_leave_configuration_and_history_unchanged(){
        let mut e=engine();e.on_start("demo",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();e.process_block(0,&trace_test_iq(16384,20.0)).unwrap();
        let frame=e.frame();let state=e.state();let config=e.configuration();
        for patch in [json!({"density_min_db":""}),json!({"density_min_db":null}),json!({"density_max_db":false}),
            json!({"density_min_db":0.0,"density_max_db":0.0}),json!({"density_min_db":10.0,"density_max_db":-10.0}),
            json!({"density_min_db":-1e308,"density_max_db":1e308}),json!({"density_levels":0}),
            json!({"density_level_unit":"dBm","density_min_db":-100.0}),json!({"density_level_unit":null}),json!({"density_level_unit":""})]{
            assert!(e.apply(&json!({"action":"set_analysis","analysis":patch})).is_err());
            assert_eq!(e.state(),state);assert_eq!(e.configuration(),config);assert_eq!(e.frame(),frame);
        }e.on_stop("test");
    }
    #[test]
    fn density_axis_canonical_units_persist_and_roundtrip_under_calibration(){
        let mut e=engine();e.apply(&json!({"action":"calibration_load","calibration":calibration_profile()})).unwrap();e.on_start("live",live_metadata()).unwrap();
        e.process_block(0,&trace_test_iq(16384,20.0)).unwrap();e.apply(&json!({"action":"set_level_unit","unit":"absolute"})).unwrap();
        e.apply(&json!({"action":"set_analysis","analysis":{"density_min_db":-100.0,"density_max_db":-10.0,"density_level_unit":"dBFS"}})).unwrap();
        let frame=e.frame();assert_eq!(frame["density"]["min_db"],-135.0);assert_eq!(frame["density"]["max_db"],-45.0);
        let state=e.state();assert_eq!(state["analysis"]["density_min_db"],-100.0);assert_eq!(state["analysis"]["density_max_db"],-10.0);assert!(state["analysis"].get("density_level_unit").is_none());
        e.on_stop("test");e.apply(&json!({"action":"preset_save","name":"density-test"})).unwrap();
        e.apply(&json!({"action":"set_analysis","analysis":{"density_min_db":-80.0,"density_max_db":-5.0}})).unwrap();
        e.apply(&json!({"action":"preset_load","name":"density-test"})).unwrap();assert_eq!(e.analyzer.config.density_min_db,-100.0);assert_eq!(e.analyzer.config.density_max_db,-10.0);
        let config=e.configuration();assert!(config["analysis"].get("density_level_unit").is_none());let project=e.project.clone();drop(e);let restored=Engine::new(project).unwrap();
        assert_eq!(restored.analyzer.config.density_min_db,-100.0);assert_eq!(restored.analyzer.config.density_max_db,-10.0);assert!(!restored.running);
    }
    #[test]
    fn input_confirmation_and_all_routes_share_filter_bounds(){
        let mut e=engine();let old=e.input.clone();let err=e.plan_input(&json!({"center_hz":40e6}),false).unwrap_err();
        assert_eq!(err.code,"VFO_IMPACT");assert_eq!(e.input,old);assert_eq!(err.impacted_vfos.len(),1);
        e.apply(&json!({"action":"set_input","input":{"center_hz":40e6},"confirm_impacted":true})).unwrap();assert!(!e.vfos[0].enabled);
        assert!(e.apply(&json!({"action":"resume_vfo","id":1})).is_err());
        assert!(e.apply(&json!({"action":"add_vfo","vfo":{"frequency_hz":40e6+e.usable()/2.0,"bandwidth_hz":10000,"mode":"AM"}})).is_err());
    }
    #[test]
    fn arm_locks_input_but_display_does_not_change_measurement_data(){
        let mut e=engine();e.on_start("demo",json!({"name":"test","center_hz":32256000,"sample_rate":2048000})).unwrap();
        e.apply(&json!({"action":"arm"})).unwrap();assert_eq!(e.plan_input(&json!({"gain_db":30}),false).unwrap_err().code,"locked");
        assert!(e.plan_input(&json!({"gain_db":20.7}),false).is_ok());
        assert!(e.apply(&json!({"action":"set_analysis","analysis":{"window":"blackman"}})).is_err());
        e.apply(&json!({"action":"set_display","display":{"ref_level":-30,"span_hz":500000}})).unwrap();
        assert_eq!(e.input["gain_db"],20.7);assert!(e.trigger.armed);e.on_stop("test");
    }
    #[test]
    fn settings_restore_never_starts_receive_arm_or_record(){
        let mut e=engine();e.apply(&json!({"action":"add_vfo","vfo":{"frequency_hz":32100000,"mode":"AM","bandwidth_hz":10000}})).unwrap();
        let project=e.project.clone();drop(e);let restored=Engine::new(project).unwrap();assert_eq!(restored.vfos.len(),2);assert!(!restored.running&&!restored.trigger.armed&&!restored.iq_active()&&!restored.wav_active());
    }
    #[test]
    fn manual_iq_and_per_vfo_wav_are_independent_and_input_change_splits(){
        let mut e=engine();e.on_start("demo",json!({"name":"test","center_hz":32256000,"sample_rate":2048000})).unwrap();
        e.apply(&json!({"action":"wav_start","id":1})).unwrap();let old=e.vfos[0].wav.as_ref().unwrap().path.clone();
        assert!(e.plan_input(&json!({"center_hz":33000000}),true).is_err());assert!(e.plan_input(&json!({"gain_db":30}),false).is_ok());
        e.on_stop("input_change");assert!(e.wav_active());e.apply(&json!({"action":"set_input","input":{"gain_db":30}})).unwrap();
        e.on_start("demo",json!({"name":"test","center_hz":32256000,"sample_rate":2048000})).unwrap();assert_ne!(old,e.vfos[0].wav.as_ref().unwrap().path);
        e.apply(&json!({"action":"iq_start"})).unwrap();assert!(e.iq_active()&&e.wav_active());
        e.apply(&json!({"action":"wav_stop","id":1})).unwrap();assert!(e.iq_active()&&!e.wav_active());e.on_stop("test");
    }
    #[test]
    fn linked_roi_and_replay_seek_lock(){
        let mut e=engine();e.apply(&json!({"action":"set_measurement","measurement":{"link_vfo":1,"obw_percent":95}})).unwrap();
        assert_eq!(e.linked_band(&e.measurement).unwrap(),(31900000.0,32100000.0));
        e.source="replay".into();assert!(e.plan_input(&json!({"ppm":1}),false).is_err());assert!(e.plan_input(&json!({"ppm":0}),false).is_ok());
        e.reset_for_seek(1000).unwrap();assert_eq!(e.last_sample,1000);
    }
    #[test]
    fn trigger_iq_zero_pre_does_not_include_earlier_current_block_samples(){
        let mut e=engine();e.on_start("demo",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();
        e.apply(&json!({"action":"set_trigger","trigger":{"pre_seconds":0,"post_seconds":0.005,"threshold_db":-50}})).unwrap();
        e.apply(&json!({"action":"arm"})).unwrap();
        let raw:Vec<u8>=(0..131072).flat_map(|k|{let phase=-std::f64::consts::TAU*256000.0*k as f64/2048000.0;
            [(127.5+30.0*phase.cos()).round()as u8,(127.5+30.0*phase.sin()).round()as u8]}).collect();
        e.process_block(0,&raw).unwrap();assert!(!e.iq_active());assert_eq!(e.iq_last["samples"],10240);
        let recorder=e.iq.as_ref().unwrap();let data=fs::read(recorder.data_path.as_ref().unwrap()).unwrap();
        let metadata:Value=serde_json::from_slice(&fs::read(recorder.meta_path.as_ref().unwrap()).unwrap()).unwrap();
        let first=metadata["captures"][0]["core:global_index"].as_u64().unwrap()as usize;
        assert!(first>0);assert_eq!(data,&raw[first*2..(first+10240)*2]);
    }
    #[test]
    fn wav_metadata_exposes_fixed_audio_gain_and_noop_update_preserves_state(){
        let mut e=engine();let metadata=e.vfo_conditions(0);assert_eq!(metadata["audio_gain"],0.5);
        assert!((metadata["audio_gain_db"].as_f64().unwrap()+6.020599913).abs()<1e-6);
        e.vfos[0].sample_count=123;e.apply(&json!({"action":"update_vfo","id":1,"patch":{"wav_format":"PCM16"}})).unwrap();assert_eq!(e.vfos[0].sample_count,123);
    }
    #[test]
    fn additional_trigger_conditions_do_not_replace_primary_metric(){
        let mut e=engine();e.apply(&json!({"action":"set_trigger","trigger":{"pre_seconds":0,"threshold_db":-20,"logic":"AND","conditions":[{"metric":"integrated","threshold_db":-60}]}})).unwrap();
        assert_eq!(e.state()["trigger"]["conditions"].as_array().unwrap().len(),1);
        e.trigger.arm(None,0.0).unwrap();assert!(e.trigger.evaluate(&json!({"peak":-40,"power":-40}),0,2048).unwrap().is_empty());
        assert_eq!(e.trigger.snapshot()["conditions"].as_array().unwrap().len(),2);
    }
    #[test]
    fn restart_and_seek_clear_performance_and_analysis_from_previous_run(){
        let mut e=engine();let metadata=json!({"sample_rate":2048000,"center_hz":32256000});
        e.on_start("demo",metadata.clone()).unwrap();e.process_block(0,&vec![128;32768]).unwrap();
        assert!(e.performance["valid_frames"].as_u64().unwrap()>0);assert!(e.last_sample>0);
        e.on_stop("test");e.on_start("demo",metadata).unwrap();
        assert_eq!(e.performance["valid_frames"],0);assert_eq!(e.performance["input_samples"],0);
        assert_eq!(e.performance["load_percent"],0.0);assert_eq!(e.analyzer.total_valid_frames,0);assert_eq!(e.last_sample,0);
        e.process_block(0,&vec![128;32768]).unwrap();e.source="replay".into();e.replay=json!({});e.reset_for_seek(1000).unwrap();
        assert_eq!(e.performance["valid_frames"],0);assert_eq!(e.performance["input_samples"],1000);
        assert_eq!(e.analyzer.total_valid_frames,0);assert_eq!(e.last_sample,1000);
    }
    fn calibration_profile()->Value{json!({"schema":"spectrum-studio-calibration-1","label":"unit test reference","offset_db":-35.0,
        "frequency_low_hz":30000000.0,"frequency_high_hz":35000000.0,"sample_rate":2048000.0,"gain_db":20.7,
        "mode":"tuner","agc":false,"device_name":"Test SDR","expires_utc":"2099-01-01T00:00:00Z","reference_note":"Synthetic fixture; no physical calibration was performed"})}
    fn live_metadata()->Value{json!({"name":"Test SDR","sample_rate":2048000.0,"center_hz":32256000.0,"gain_db_actual":20.7,"agc":false,"mode_actual":"tuner"})}
    #[test]
    fn calibrated_display_conversion_preserves_canonical_psd_thresholds_and_settings(){
        let mut e=engine();e.apply(&json!({"action":"calibration_load","calibration":calibration_profile()})).unwrap();
        assert!(!e.state()["calibration"]["valid"].as_bool().unwrap());assert!(e.apply(&json!({"action":"set_level_unit","unit":"absolute"})).is_err());
        e.on_start("live",live_metadata()).unwrap();assert!(e.state()["calibration"]["valid"].as_bool().unwrap());
        e.process_block(0,&vec![128;32768]).unwrap();let relative=e.frame();let psd=e.analyzer.last_psd.clone();let threshold=e.trigger_config["threshold_db"].clone();
        e.apply(&json!({"action":"set_level_unit","unit":"absolute"})).unwrap();let absolute=e.frame();
        assert_eq!(e.state()["display"]["ref_level"],-55.0);assert_eq!(e.state()["display"]["ref_level_dbfs"],-20.0);
        assert_eq!(absolute["spectrum"]["unit"],"dBm");
        assert!((absolute["spectrum"]["live_db"][0].as_f64().unwrap()-relative["spectrum"]["live_db"][0].as_f64().unwrap()+35.0).abs()<1e-8);
        assert_eq!(absolute["measurements"]["cp_dbfs"],relative["measurements"]["cp_dbfs"]);assert_eq!(e.analyzer.last_psd,psd);assert_eq!(e.trigger_config["threshold_db"],threshold);
        e.apply(&json!({"action":"set_display","display":{"ref_level":-65.0}})).unwrap();assert_eq!(e.display["ref_level"],-30.0);assert_eq!(e.analyzer.config.ref_level,-30.0);
        let config=e.configuration();assert_eq!(config["display"]["ref_level"],-30.0);assert!(config["input"].get("gain_actual_db").is_none());
        e.on_stop("test");assert_eq!(e.state()["calibration"]["unit"],"dBFS");assert!(!e.state()["calibration"]["valid"].as_bool().unwrap());
        e.apply(&json!({"action":"set_display","display":{"ref_level":-70.0,"ref_level_unit":"dBm"}})).unwrap();assert_eq!(e.display["ref_level"],-35.0);
        assert_eq!(e.state()["calibration"]["unit"],"dBFS");assert!(e.display.get("ref_level_unit").is_none());
        let project=e.project.clone();drop(e);let restored=Engine::new(project).unwrap();assert!(restored.calibration.is_some());
        assert_eq!(restored.requested_level_unit,"absolute");assert!(!restored.running);assert_eq!(restored.state()["calibration"]["unit"],"dBFS");
    }
    #[test]
    fn source_gain_agc_and_measurement_changes_invalidate_absolute_units(){
        let mut e=engine();e.apply(&json!({"action":"calibration_load","calibration":calibration_profile()})).unwrap();
        e.on_start("demo",live_metadata()).unwrap();assert!(!e.state()["calibration"]["valid"].as_bool().unwrap());e.on_stop("test");
        e.on_start("live",live_metadata()).unwrap();e.apply(&json!({"action":"set_level_unit","unit":"absolute"})).unwrap();
        e.input["gain_actual_db"]=json!(28.0);assert_eq!(e.state()["calibration"]["unit"],"dBFS");e.input["gain_actual_db"]=json!(20.7);
        e.input["agc"]=json!(true);assert!(!e.state()["calibration"]["valid"].as_bool().unwrap());e.input["agc"]=json!(false);
        e.apply(&json!({"action":"set_measurement","measurement":{"low_hz":29000000.0}})).unwrap();assert_eq!(e.state()["calibration"]["unit"],"dBFS");
        e.apply(&json!({"action":"calibration_clear"})).unwrap();assert!(!e.state()["calibration"]["loaded"].as_bool().unwrap());assert_eq!(e.requested_level_unit,"relative");
        assert!(e.apply(&json!({"action":"set_display","display":{"ref_level":-70.0,"ref_level_unit":"dBm"}})).is_err());
    }
    #[test]
    fn fft_configuration_clear_synchronizes_performance_immediately(){
        let mut e=engine();e.on_start("demo",json!({"sample_rate":2048000,"center_hz":32256000})).unwrap();e.process_block(0,&vec![128;32768]).unwrap();
        assert!(e.performance["valid_frames"].as_u64().unwrap()>0);
        e.apply(&json!({"action":"set_analysis","analysis":{"window":"blackman"}})).unwrap();
        assert_eq!(e.analyzer.total_valid_frames,0);assert_eq!(e.performance["valid_frames"],0);
    }
    fn marker_tones(e:&mut Engine){
        let tones=[(-300000.0,0.20),(200000.0,0.35),(600000.0,0.12)];let fs=e.fs();
        let values:Vec<Complex32>=(0..e.analyzer.config.fft_size).map(|sample|{
            let mut value=Complex32::new(0.0,0.0);
            for(offset,amplitude)in tones{let phase=std::f64::consts::TAU*offset*sample as f64/fs;
                value+=Complex32::new((amplitude*phase.cos())as f32,(amplitude*phase.sin())as f32);
            }value
        }).collect();
        e.analyzer.process(&values,0);e.last_sample=values.len()as u64;
    }
    #[test]
    fn legacy_user_markers_get_stable_ids_and_selection_survives_settings_restore(){
        let mut e=engine();e.apply(&json!({"action":"set_measurement","measurement":{"manual_markers":[{"frequency_hz":32000000.0},{"id":7,"frequency_hz":32500000.0},32200000.0],"selected_user_marker_id":7}})).unwrap();
        assert_eq!(e.measurement["manual_markers"],json!([{"id":1,"frequency_hz":32000000.0},{"id":7,"frequency_hz":32500000.0},{"id":2,"frequency_hz":32200000.0}]));
        e.apply(&json!({"action":"user_marker_add","frequency_hz":32300000.0})).unwrap();assert_eq!(e.measurement["selected_user_marker_id"],8);
        e.apply(&json!({"action":"user_marker_select","id":7})).unwrap();let original=e.measurement.clone();
        assert!(e.apply(&json!({"action":"set_measurement","measurement":{"manual_markers":[{"id":1,"frequency_hz":32000000},{"id":1,"frequency_hz":32100000}]}})).is_err());assert_eq!(e.measurement,original);
        let project=e.project.clone();drop(e);let restored=Engine::new(project).unwrap();assert_eq!(restored.measurement,original);
        assert!(!restored.running);assert_eq!(restored.measurement["selected_user_marker_id"],7);
    }
    #[test]
    fn user_selection_and_multiple_markers_leave_automatic_markers_running(){
        let mut e=engine();marker_tones(&mut e);e.apply(&json!({"action":"set_measurement","measurement":{"marker_count":1}})).unwrap();
        let before=e.frame();let automatic=before["measurements"]["markers"][0]["frequency_hz"].clone();assert!(automatic.is_number());
        e.apply(&json!({"action":"user_marker_add","frequency_hz":32000000.0})).unwrap();e.apply(&json!({"action":"user_marker_add","frequency_hz":32300000.0})).unwrap();
        e.apply(&json!({"action":"user_marker_select","id":1})).unwrap();let frame=e.frame();let markers=frame["measurements"]["markers"].as_array().unwrap();
        assert_eq!(markers.len(),3);assert_eq!(markers[0]["kind"],"auto");assert_eq!(markers[0]["label"],"A1");assert_eq!(markers[0]["frequency_hz"],automatic);assert_eq!(markers[0]["selected"],false);
        assert_eq!(markers[1]["label"],"U1");assert_eq!(markers[1]["selected"],true);assert_eq!(markers[2]["label"],"U2");assert_eq!(markers[2]["selected"],false);
        assert_eq!(e.measurement["marker_mode"],"auto");assert_eq!(e.measurement["marker_follow"],true);
        e.apply(&json!({"action":"user_marker_delete","id":1})).unwrap();assert!(e.measurement["selected_user_marker_id"].is_null());assert_eq!(e.measurement["manual_markers"][0]["id"],2);
        e.apply(&json!({"action":"user_marker_select","id":2})).unwrap();e.apply(&json!({"action":"user_marker_select","id":null})).unwrap();assert!(e.measurement["selected_user_marker_id"].is_null());
    }
    #[test]
    fn user_peak_navigation_uses_all_visible_candidates_independently_of_roi_and_auto_count(){
        let mut e=engine();marker_tones(&mut e);e.apply(&json!({"action":"set_measurement","measurement":{"marker_count":1}})).unwrap();
        let center=e.center();e.apply(&json!({"action":"user_marker_add"})).unwrap();
        let frequency=|engine:&Engine|engine.measurement["manual_markers"][0]["frequency_hz"].as_f64().unwrap();
        assert!((frequency(&e)-(center+200000.0)).abs()<1.0);
        e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"right"})).unwrap();assert!((frequency(&e)-(center+600000.0)).abs()<1.0);
        let last=frequency(&e);assert_eq!(e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"right"})).unwrap_err().code,"PEAK_BOUNDARY");assert_eq!(frequency(&e),last);
        e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"left"})).unwrap();assert!((frequency(&e)-(center+200000.0)).abs()<1.0);
        e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"left"})).unwrap();assert!((frequency(&e)-(center-300000.0)).abs()<1.0);
        assert_eq!(e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"left"})).unwrap_err().code,"PEAK_BOUNDARY");
        e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"max"})).unwrap();assert!((frequency(&e)-(center+200000.0)).abs()<1.0);
        e.apply(&json!({"action":"set_display","display":{"span_hz":500000.0}})).unwrap();
        e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"max"})).unwrap();assert!((frequency(&e)-(center+200000.0)).abs()<1.0);
        assert_eq!(e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"right"})).unwrap_err().code,"PEAK_BOUNDARY");
        e.apply(&json!({"action":"set_display","display":{"span_hz":1800000.0}})).unwrap();
        e.apply(&json!({"action":"set_measurement","measurement":{"low_hz":10000000.0,"high_hz":11000000.0}})).unwrap();
        e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"max"})).unwrap();assert!((frequency(&e)-(center+200000.0)).abs()<1.0);
        let mut flat=engine();let zeros=vec![Complex32::new(0.0,0.0);flat.analyzer.config.fft_size];flat.analyzer.process(&zeros,0);
        assert_eq!(flat.apply(&json!({"action":"user_marker_add"})).unwrap_err().code,"NO_PEAK");
    }
    #[test]
    fn user_marker_frequency_bounds_missing_fft_and_center_changes_do_not_invent_levels(){
        let mut e=engine();let center=e.center();let edge=center+e.usable()/2.0;
        assert_eq!(e.apply(&json!({"action":"user_marker_add"})).unwrap_err().code,"NO_DATA");
        e.apply(&json!({"action":"user_marker_add","frequency_hz":edge})).unwrap();let no_data=e.frame();let marker=&no_data["measurements"]["markers"][0];
        assert_eq!(marker["status"],"no_data");assert!(marker["level_dbfs"].is_null());assert!(marker["level_dbm"].is_null());assert_eq!(marker["offset_hz"],e.usable()/2.0);
        assert_eq!(e.apply(&json!({"action":"user_marker_peak","id":1,"direction":"max"})).unwrap_err().code,"NO_DATA");
        assert_eq!(e.apply(&json!({"action":"user_marker_move","id":1,"frequency_hz":edge+1.0})).unwrap_err().code,"OUT_OF_BAND");
        assert_eq!(e.measurement["manual_markers"][0]["frequency_hz"],edge);assert_eq!(e.center(),center);
        e.apply(&json!({"action":"user_marker_move","id":1,"frequency_hz":center-e.usable()/2.0})).unwrap();
        let fixed=e.measurement["manual_markers"][0]["frequency_hz"].clone();e.apply(&json!({"action":"set_input","input":{"center_hz":40000000.0},"confirm_impacted":true})).unwrap();
        let moved_center=e.frame();let marker=&moved_center["measurements"]["markers"][0];assert_eq!(marker["frequency_hz"],fixed);assert_eq!(marker["status"],"out_of_band");assert!(marker["level_dbfs"].is_null());assert!(marker["delta_db"].is_null());assert_eq!(marker["selected"],true);
        assert_eq!(e.apply(&json!({"action":"user_marker_select","id":99})).unwrap_err().code,"MARKER_NOT_FOUND");
    }
    #[test]
    fn user_marker_calibration_keeps_canonical_level_id_and_offset(){
        let mut e=engine();e.apply(&json!({"action":"calibration_load","calibration":calibration_profile()})).unwrap();e.on_start("live",live_metadata()).unwrap();marker_tones(&mut e);
        e.apply(&json!({"action":"user_marker_add","frequency_hz":32456000.0})).unwrap();let relative=e.frame();
        let reference=relative["measurements"]["markers"].as_array().unwrap().iter().find(|marker|marker["kind"]=="user").unwrap().clone();
        e.apply(&json!({"action":"set_level_unit","unit":"absolute"})).unwrap();let absolute=e.frame();let marker=absolute["measurements"]["markers"].as_array().unwrap().iter().find(|marker|marker["kind"]=="user").unwrap();
        assert_eq!(marker["level_dbfs"],reference["level_dbfs"]);assert!((marker["level_dbm"].as_f64().unwrap()-reference["level_dbfs"].as_f64().unwrap()+35.0).abs()<1e-8);
        assert_eq!(marker["id"],1);assert_eq!(marker["offset_hz"],200000.0);assert_eq!(marker["selected"],true);
    }
    fn assert_roi_trigger_matches_cp(low_bins:f64,high_bins:f64,tone_bin:f64,sample_rate:f64){
        let mut prototype=engine();prototype.apply(&json!({"action":"set_input","input":{"sample_rate":sample_rate}})).unwrap();
        let n=prototype.analyzer.config.fft_size;let center=prototype.center();let bin=prototype.analyzer.bin_hz;
        let low=center+low_bins*bin;let high=center+high_bins*bin;
        assert!(prototype.band_valid(low,high));
        let raw:Vec<u8>=(0..n*2).flat_map(|sample|{let phase=std::f64::consts::TAU*tone_bin*sample as f64/n as f64;
            [(127.5+25.0*phase.cos()).round()as u8,(127.5+25.0*phase.sin()).round()as u8]}).collect();
        let values=crate::device::decode(&raw);let mut reference=Analyzer::new(prototype.analyzer.config.clone());reference.process(&values,0);
        let cp=reference.measurements(low,high,center,99.0,1)["cp_dbfs"].as_f64().unwrap();
        let weights=prototype.trigger_band_weights(low,high);
        assert!((weights.iter().map(|(_,width)|width).sum::<f64>()-(high-low)).abs()<1e-8);
        let(peak,power)=Engine::trigger_band_powers(&reference.last_psd,&weights,reference.enbw_hz);
        assert!((db(power)-cp).abs()<1e-10,"Trigger integration must match fractional-bin Channel Power");
        // Bracket both thresholds. An over-counted edge bin must not trip the
        // upper threshold; a sub-bin ROI must not silently become zero power.
        for(metric,expected)in [("integrated",cp),("peak",db(peak))]{for margin in [-0.05,0.05]{
            let mut e=engine();e.apply(&json!({"action":"set_input","input":{"sample_rate":sample_rate}})).unwrap();
            e.on_start("demo",json!({"center_hz":center,"sample_rate":sample_rate})).unwrap();
            e.apply(&json!({"action":"set_trigger","trigger":{"pre_seconds":0.0,"post_seconds":0.05,"metric":metric,
                "low_hz":low,"high_hz":high,"threshold_db":expected+margin}})).unwrap();
            e.apply(&json!({"action":"arm"})).unwrap();e.process_block(0,&raw).unwrap();
            assert_eq!(e.iq_active(),margin<0.0,"{metric} ROI [{low},{high}] threshold margin {margin}");
            e.on_stop("boundary_test_complete");
        }}
    }
    #[test]
    fn trigger_sub_bin_roi_matches_cp_and_peak_without_a_bin_center_inside(){
        let e=engine();let bin=e.analyzer.bin_hz;let weights=e.trigger_band_weights(e.center()+0.25*bin,e.center()+0.75*bin);
        assert_eq!(weights.len(),2);assert!((weights[0].1-0.25*bin).abs()<1e-8);assert!((weights[1].1-0.25*bin).abs()<1e-8);
        assert_roi_trigger_matches_cp(0.25,0.75,0.0,2048000.0);
    }
    #[test]
    fn trigger_exact_bin_endpoints_do_not_double_count_or_include_zero_width_neighbors(){
        let e=engine();let bin=e.analyzer.bin_hz;let weights=e.trigger_band_weights(e.center(),e.center()+bin);
        assert_eq!(weights.len(),2);assert!((weights[0].1-bin/2.0).abs()<1e-8);assert!((weights[1].1-bin/2.0).abs()<1e-8);
        let whole=e.trigger_band_weights(e.center()-bin/2.0,e.center()+bin/2.0);assert_eq!(whole,vec![(e.analyzer.config.fft_size/2,bin)]);
        let mut psd=vec![0.0;e.analyzer.config.fft_size];psd[e.analyzer.config.fft_size/2]=1.0;psd[e.analyzer.config.fft_size/2+1]=100.0;
        assert_eq!(Engine::trigger_band_powers(&psd,&whole,e.analyzer.enbw_hz),(e.analyzer.enbw_hz,bin));
        assert_roi_trigger_matches_cp(0.0,1.0,0.0,2048000.0);
    }
    #[test]
    fn trigger_partial_bins_at_both_usable_band_edges_match_cp_at_max_sample_rate(){
        let edge=4096.0f64*0.45;
        assert_roi_trigger_matches_cp(edge-0.6,edge,edge.floor(),2400000.0);
        assert_roi_trigger_matches_cp(-edge,-edge+0.6,-edge.floor(),2400000.0);
    }
}
