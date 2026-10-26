//! Explicit saved-IQ check of the production Engine; no device, HTTP or app I/O.
//! Normal `cargo test` reports this fixture-dependent check as ignored.
//!
//! Required environment variables (absolute paths inside this project):
//! STUDIO_MARKER_COMPARISON_JSON = verification/.../dc-on-off-comparison.json
//! STUDIO_MARKER_PRESET_JSON     = verification/.../measurement-optimized-preset.json
//! Run explicitly with:
//! cargo test --release --test offline_user_marker_fixture -- --ignored --nocapture
//! Each run preserves its report and isolated settings under verification/.
use chrono::Utc;
use rtl_spectrum_studio::{engine::Engine,recording::ReplayReader};
use rustfft::{num_complex::Complex64,FftPlanner};
use serde_json::{json,Value};
use std::{env,fs::{self,OpenOptions},io::Write,path::{Path,PathBuf}};

fn read_json(path:&Path)->Result<Value,String>{
    let bytes=fs::read(path).map_err(|e|e.to_string())?;
    let bytes=bytes.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).map_err(|e|e.to_string())
}
fn required_path(name:&str,project:&Path)->Result<PathBuf,String>{
    let value=env::var_os(name).ok_or_else(||format!("Set {name} explicitly; this test does not substitute synthetic data."))?;
    within_project(&PathBuf::from(value),project)
}
fn within_project(path:&Path,project:&Path)->Result<PathBuf,String>{
    let resolved=path.canonicalize().map_err(|e|e.to_string())?;
    if !resolved.starts_with(project){return Err("Fixture paths must remain inside this project".into());}Ok(resolved)
}
fn exclusive_json(path:&Path,value:&Value)->Result<(),String>{
    let mut file=OpenOptions::new().write(true).create_new(true).open(path).map_err(|e|e.to_string())?;
    let bytes=serde_json::to_vec_pretty(value).map_err(|e|e.to_string())?;
    file.write_all(&bytes).and_then(|_|file.write_all(b"\n")).and_then(|_|file.sync_all()).map_err(|e|e.to_string())
}
fn number(value:&Value,key:&str)->Result<f64,String>{value[key].as_f64().filter(|n|n.is_finite()).ok_or_else(||format!("Missing finite {key}"))}
fn record(report:&mut Value,name:&str,passed:bool,detail:Value){report["checks"].as_array_mut().unwrap().push(json!({"check":name,"passed":passed,"detail":detail}));}
fn action(engine:&mut Engine,body:Value)->Result<Value,String>{engine.apply(&body).map_err(|e|format!("{}: {}",e.code,e.message))}
fn user_marker(frame:&Value,id:u64)->Result<Value,String>{
    frame["measurements"]["markers"].as_array().and_then(|markers|markers.iter().find(|marker|marker["kind"]=="user"&&marker["id"].as_u64()==Some(id)))
        .cloned().ok_or_else(||format!("Missing user marker {id}"))
}
fn automatic_markers(frame:&Value)->Vec<Value>{
    frame["measurements"]["markers"].as_array().into_iter().flatten().filter(|marker|marker["kind"]=="auto").cloned().collect()
}
fn identifier(bytes:&[u8])->String{
    let mut hash=0xcbf29ce484222325u64;for byte in bytes{hash^=*byte as u64;hash=hash.wrapping_mul(0x100000001b3);}format!("{hash:016x}")
}

/// Independent f64 FFT of the last complete source-contiguous frame. The
/// standard PSD × ENBW expression reduces to |FFT|²/(sum(window))² here.
fn reference_levels(raw:&[u8],reader:&ReplayReader,state:&Value)->Result<(Vec<f64>,f64,usize),String>{
    let analysis=&state["analysis"];let n=analysis["fft_size"].as_u64().ok_or("Missing FFT size")? as usize;
    let fs=number(analysis,"sample_rate")?;let hop=(n as f64*(1.0-number(analysis,"overlap")?)).round().max(1.0)as usize;
    let segment=reader.captures.last().and_then(|capture|capture["core:sample_start"].as_u64()).unwrap_or(0)as usize;
    let samples=raw.len()/2;if samples<segment+n{return Err("Fixture has no complete final contiguous FFT frame".into());}
    let first=segment+(samples-segment-n)/hop*hop;
    let window=analysis["window"].as_str().ok_or("Missing FFT window")?;
    let mut sum=0.0;let mut fft=Vec::with_capacity(n);
    for index in 0..n{
        let angle=std::f64::consts::TAU*index as f64/n as f64;
        let weight=match window{"hann"|"hanning"=>0.5-0.5*angle.cos(),"hamming"=>0.54-0.46*angle.cos(),"blackman"=>0.42-0.5*angle.cos()+0.08*(2.0*angle).cos(),"rect"|"rectangular"|"boxcar"|"none"=>1.0,_=>return Err("Unsupported reference window".into())};
        sum+=weight;let pair=&raw[(first+index)*2..(first+index+1)*2];
        fft.push(Complex64::new((pair[0]as f64-127.5)/127.5*weight,(pair[1]as f64-127.5)/127.5*weight));
    }
    FftPlanner::<f64>::new().plan_fft_forward(n).process(&mut fft);
    let levels=(0..n).map(|index|10.0*(fft[(index+n/2)%n].norm_sqr()/(sum*sum)).max(1e-30).log10()).collect();
    Ok((levels,fs/n as f64,first))
}

fn execute(project:&Path,workspace:&Path,comparison_path:&Path,preset_path:&Path,report:&mut Value)->Result<(),String>{
    let comparison=read_json(comparison_path)?;let preset=read_json(preset_path)?;
    let capture=comparison["captures"].as_array().and_then(|captures|captures.iter().find(|capture|capture["label"]=="on_before"))
        .ok_or("Comparison JSON has no on_before capture")?;
    if capture["output_on"].as_str()!=Some("1"){return Err("Chosen capture is not labelled output ON".into());}
    let meta_path=within_project(Path::new(capture["recording"]["meta_path"].as_str().ok_or("Missing metadata path")?),project)?;
    let data_path=within_project(Path::new(capture["recording"]["data_path"].as_str().ok_or("Missing raw IQ path")?),project)?;
    let mut reader=ReplayReader::new(&meta_path)?;
    if reader.data_path.canonicalize().map_err(|e|e.to_string())?!=data_path{return Err("Comparison and SigMF refer to different datasets".into());}
    let configuration=if preset["configuration"].is_object(){preset["configuration"].clone()}else{preset.clone()};
    let settings=workspace.join("data/settings/presets");fs::create_dir_all(&settings).map_err(|e|e.to_string())?;
    exclusive_json(&settings.join("offline-fixture.json"),&configuration)?;
    let mut engine=Engine::new(workspace.to_path_buf())?;
    action(&mut engine,json!({"action":"preset_load","name":"offline-fixture"}))?;
    let loaded=engine.state();record(report,"optimized_preset_restored_without_start",!loaded["running"].as_bool().unwrap_or(true)&&loaded["analysis"]["fft_size"]==configuration["analysis"]["config"]["fft_size"]&&loaded["display"]["span_hz"]==configuration["display"]["span_hz"],json!({"analysis":loaded["analysis"],"display":loaded["display"]}));
    engine.on_start("replay",json!({"name":"Saved ON IQ fixture, offline only","sample_rate":reader.sample_rate,"center_hz":reader.center_hz,"input_metadata":reader.input_metadata}))?;
    let mut raw_all=Vec::new();
    while reader.position_samples<reader.total_samples{
        let position=reader.position_samples;
        let next=reader.captures.iter().filter_map(|capture|capture["core:sample_start"].as_u64()).filter(|start|*start>position).min().unwrap_or(reader.total_samples);
        let count=131072usize.min((next-position)as usize);let global=reader.global_sample_for(position);
        let(_,raw)=reader.read_chunk(count)?;if raw.is_empty(){return Err("Unexpected empty IQ fixture block".into());}
        engine.process_block(global,&raw)?;raw_all.extend_from_slice(&raw);
    }
    let state=engine.state();let center=number(&state["input"],"center_hz")?;
    let(initial_levels,bin,frame_start)=reference_levels(&raw_all,&reader,&state)?;
    let span=number(&state["display"],"span_hz")?.min(number(&state["input"],"usable_bandwidth_hz")?);
    let n=initial_levels.len();let frequency=|index:usize|center+(index as f64-n as f64/2.0)*bin;
    let strongest=(0..n).filter(|index|(frequency(*index)-center).abs()<=span/2.0).max_by(|a,b|initial_levels[*a].total_cmp(&initial_levels[*b])).ok_or("No reference bins in span")?;
    let nearest=|frequency:f64|((frequency-center)/bin+n as f64/2.0).round().clamp(0.0,(n-1)as f64)as usize;
    let before=engine.frame();let automatic=automatic_markers(&before);
    record(report,"raw_fixture_length_and_source",raw_all.len()as u64==reader.total_samples*2&&capture["recording"]["bytes"].as_u64()==Some(raw_all.len()as u64),json!({"bytes":raw_all.len(),"samples":reader.total_samples,"source":"saved_sigmf_cu8","reference_frame_start_file_sample":frame_start}));
    record(report,"automatic_markers_available_before_user_selection",!automatic.is_empty(),json!({"count":automatic.len()}));
    action(&mut engine,json!({"action":"user_marker_add"}))?;
    let id=engine.state()["measurement"]["selected_user_marker_id"].as_u64().ok_or("New user marker was not selected")?;
    let maximum=user_marker(&engine.frame(),id)?;let maximum_hz=number(&maximum,"frequency_hz")?;
    record(report,"default_user_marker_finds_visible_maximum",(maximum_hz-frequency(strongest)).abs()<=bin*0.51&&(number(&maximum,"level_dbfs")?-initial_levels[nearest(maximum_hz)]).abs()<0.02,json!({"user":maximum,"reference_bin_frequency_hz":frequency(strongest),"reference_level_dbfs":initial_levels[strongest],"level_tolerance_db":0.02}));
    action(&mut engine,json!({"action":"user_marker_peak","id":id,"direction":"left"}))?;
    let left=user_marker(&engine.frame(),id)?;
    action(&mut engine,json!({"action":"user_marker_peak","id":id,"direction":"right"}))?;
    let from_left=user_marker(&engine.frame(),id)?;
    action(&mut engine,json!({"action":"user_marker_peak","id":id,"direction":"right"}))?;
    let right=user_marker(&engine.frame(),id)?;
    action(&mut engine,json!({"action":"user_marker_peak","id":id,"direction":"left"}))?;
    let from_right=user_marker(&engine.frame(),id)?;
    record(report,"left_right_are_adjacent_and_return_to_same_peak",number(&left,"frequency_hz")?<maximum_hz&&number(&right,"frequency_hz")?>maximum_hz&&(number(&from_left,"frequency_hz")?-maximum_hz).abs()<1e-6&&(number(&from_right,"frequency_hz")?-maximum_hz).abs()<1e-6,json!({"left":left,"right":right,"returned_frequency_hz":number(&from_right,"frequency_hz")?}));
    action(&mut engine,json!({"action":"user_marker_peak","id":id,"direction":"max"}))?;
    let manual_hz=center-123456.0;action(&mut engine,json!({"action":"user_marker_move","id":id,"frequency_hz":manual_hz}))?;
    let manual_frame=engine.frame();let manual=user_marker(&manual_frame,id)?;
    record(report,"manual_frequency_level_and_center_offset_match_raw_fft",(number(&manual,"frequency_hz")?-manual_hz).abs()<1e-6&&(number(&manual,"offset_hz")?+123456.0).abs()<1e-6&&(number(&manual,"level_dbfs")?-initial_levels[nearest(manual_hz)]).abs()<0.02&&manual["selected"]==true,json!({"user":manual,"reference_nearest_bin_frequency_hz":frequency(nearest(manual_hz)),"reference_level_dbfs":initial_levels[nearest(manual_hz)]}));
    action(&mut engine,json!({"action":"user_marker_add","frequency_hz":center}))?;
    let second=engine.state()["measurement"]["selected_user_marker_id"].as_u64().ok_or("Second marker was not selected")?;
    action(&mut engine,json!({"action":"user_marker_select","id":id}))?;
    let final_frame=engine.frame();let final_state=engine.state();
    let users=final_frame["measurements"]["markers"].as_array().into_iter().flatten().filter(|marker|marker["kind"]=="user").count();
    record(report,"multiple_users_and_auto_markers_coexist_without_input_or_auto_mode_change",second!=id&&users==2&&automatic_markers(&final_frame)==automatic&&final_state["measurement"]["marker_mode"]==loaded["measurement"]["marker_mode"]&&final_state["measurement"]["marker_follow"]==loaded["measurement"]["marker_follow"]&&number(&final_state["input"],"center_hz")?==center&&number(&final_state["input"],"sample_rate")?==reader.sample_rate,json!({"user_count":users,"auto_count":automatic.len(),"selected_user_marker_id":id}));
    engine.on_stop("offline_fixture_complete");
    report["fixture"]=json!({"comparison":comparison_path,"preset":preset_path,"label":"on_before","metadata":meta_path,"data":data_path,"declared_data_sha256":capture["sha256"],"raw_fnv1a64":identifier(&raw_all),"sample_rate":reader.sample_rate,"center_hz":reader.center_hz,"duration_seconds":reader.duration_seconds()});
    report["final_markers"]=final_frame["measurements"]["markers"].clone();Ok(())
}

#[test]
#[ignore="requires explicitly selected saved ON IQ and optimized preset; no substitute fixture"]
fn saved_on_iq_user_marker_public_engine_api(){
    let project=PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().canonicalize().unwrap();
    let comparison=required_path("STUDIO_MARKER_COMPARISON_JSON",&project).expect("Explicit saved fixture required");
    let preset=required_path("STUDIO_MARKER_PRESET_JSON",&project).expect("Explicit optimized preset required");
    let verification=project.join("verification").canonicalize().unwrap();assert!(verification.starts_with(&project));
    let workspace=verification.join(format!("offline-user-marker-{}-{}",Utc::now().timestamp_nanos_opt().unwrap(),std::process::id()));
    fs::create_dir(&workspace).expect("A new isolated verification workspace is required");
    let workspace=workspace.canonicalize().unwrap();assert!(workspace.starts_with(&verification));assert_ne!(workspace,project);
    let mut report=json!({"schema":"spectrum-studio-offline-user-marker-fixture-1","checked_at_utc":Utc::now().to_rfc3339(),"engine_workspace":workspace,
        "hardware_io":false,"http_io":false,"running_app_changed":false,"production_settings_touched":false,"checks":[],
        "limitations":["Uses saved RF IQ; does not validate the GUI or a new live acquisition.","No calibrated RF level, sensitivity, or S/N improvement is claimed.","The declared SHA256 is provenance from the comparison manifest; this test additionally records an FNV input identifier."]});
    let outcome=execute(&project,&workspace,&comparison,&preset,&mut report);
    report["error"]=json!(outcome.as_ref().err());
    let passed=outcome.is_ok()&&report["checks"].as_array().unwrap().iter().all(|check|check["passed"]==true);
    report["passed"]=json!(passed);let output=workspace.join("offline-user-marker-result.json");exclusive_json(&output,&report).unwrap();
    println!("Offline marker result: {}",output.display());assert!(passed,"Offline saved-IQ marker check failed; see {}",output.display());
}
