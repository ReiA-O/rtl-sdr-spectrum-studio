//! Bounded cu8 SigMF recording/replay and independent PCM16/PCM24 WAV files.
//! SigMF 1.2.6: https://sigmf.org/. Optional `spectrum_studio` 0.1.0 fields
//! preserve input settings, source sample indices, gaps and finalization reason.
//! No receiver/instrument settings are changed in this module.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn positive(number: f64, name: &str) -> Result<f64, String> {
    if !number.is_finite() || number <= 0.0 {
        return Err(format!("{name} must be positive and finite"));
    }
    Ok(number)
}

fn nonnegative(number: f64, name: &str) -> Result<f64, String> {
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{name} must be nonnegative and finite"));
    }
    Ok(number)
}

fn stem(prefix: &str) -> String {
    let safe: String = prefix.chars().take(48).map(|c| {
        if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }
    }).collect();
    format!("{}_{}_{}_{}", safe, Utc::now().format("%Y%m%dT%H%M%S_%fZ"),
            std::process::id(), FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

fn atomic_json(path: &Path, data: &Value, first: bool) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(data).map_err(|e| e.to_string())?;
    if first {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| e.to_string())?;
        file.write_all(&bytes).and_then(|_| file.write_all(b"\n")).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let temporary = path.with_file_name(format!("{}.{}.tmp", path.file_name().unwrap().to_string_lossy(), stem("meta")));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&bytes).and_then(|_| file.write_all(b"\n")).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
    drop(file);
    // std::fs::rename replaces an existing regular destination on Windows too.
    fs::rename(&temporary, path).map_err(|e| e.to_string())
}

#[derive(Debug)]
pub struct PreTriggerBuffer {
    sample_rate: f64,
    max_bytes: usize,
    chunks: VecDeque<(u64, Vec<u8>)>,
    bytes_used: usize,
    pub gap_count: u64,
}

impl PreTriggerBuffer {
    pub fn new(sample_rate: f64, max_bytes: usize) -> Result<Self, String> {
        positive(sample_rate, "sample_rate")?;
        let max_bytes = max_bytes / 2 * 2;
        if max_bytes < 2 { return Err("pre-trigger capacity must hold an I/Q pair".into()); }
        Ok(Self { sample_rate, max_bytes, chunks: VecDeque::new(), bytes_used: 0, gap_count: 0 })
    }
    pub fn available_seconds(&self) -> f64 { self.bytes_used as f64 / 2.0 / self.sample_rate }
    pub fn capacity_seconds(&self) -> f64 { self.max_bytes as f64 / 2.0 / self.sample_rate }
    pub fn bytes_used(&self) -> usize { self.bytes_used }
    pub fn clear(&mut self) { self.chunks.clear(); self.bytes_used = 0; self.gap_count = 0; }
    pub fn append(&mut self, start_sample: u64, raw: &[u8]) -> Result<(), String> {
        if raw.len() % 2 != 0 { return Err("incomplete interleaved I/Q pair".into()); }
        if raw.is_empty() { return Ok(()); }
        let mut start = start_sample;
        let mut data = raw;
        if let Some((last_start, last_data)) = self.chunks.back() {
            let expected = last_start.checked_add((last_data.len()/2) as u64).ok_or("sample index overflow")?;
            if start < expected {
                let covered = (expected-start).min((data.len()/2) as u64) as usize;
                data = &data[covered*2..];
                start += covered as u64;
                if data.is_empty() { return Ok(()); }
            } else if start > expected {
                self.chunks.clear(); self.bytes_used = 0; self.gap_count += 1;
            }
        }
        if data.len() > self.max_bytes {
            let skip = data.len()-self.max_bytes;
            data = &data[skip..]; start += (skip/2) as u64;
        }
        self.chunks.push_back((start, data.to_vec())); self.bytes_used += data.len();
        while self.bytes_used > self.max_bytes {
            let (head_start, mut head_data) = self.chunks.pop_front().unwrap();
            let excess = self.bytes_used-self.max_bytes;
            if head_data.len() <= excess { self.bytes_used -= head_data.len(); }
            else {
                head_data.drain(..excess);
                self.chunks.push_front((head_start+(excess/2) as u64, head_data));
                self.bytes_used -= excess;
            }
        }
        Ok(())
    }
    /// Samples at/after `end_sample` are excluded; availability is actual data.
    pub fn get(&self, seconds: f64, end_sample: Option<u64>) -> Result<(Vec<(u64, Vec<u8>)>, f64), String> {
        nonnegative(seconds, "seconds")?;
        let count = (seconds*self.sample_rate) as u64;
        if self.chunks.is_empty() || count == 0 { return Ok((vec![], 0.0)); }
        let (last_start, last_data) = self.chunks.back().unwrap();
        let newest_end = last_start.saturating_add((last_data.len()/2) as u64);
        let end = end_sample.unwrap_or(newest_end).min(newest_end);
        let first = self.chunks.front().unwrap().0.max(end.saturating_sub(count));
        let mut chunks = Vec::new();
        let mut samples = 0_u64;
        for (start, data) in &self.chunks {
            let low = first.max(*start);
            let high = end.min(start.saturating_add((data.len()/2) as u64));
            if high > low {
                chunks.push((low, data[((low-start)*2) as usize..((high-start)*2) as usize].to_vec()));
                samples += high-low;
            }
        }
        Ok((chunks, samples as f64/self.sample_rate))
    }
}

pub struct IQRecorder {
    output_dir: PathBuf,
    pub sample_rate: f64,
    pub center_hz: f64,
    pub input_metadata: Value,
    pub max_bytes: u64,
    pub max_seconds: f64,
    pub active: bool,
    pub limit_reached: bool,
    pub last_error: Option<String>,
    pub stop_reason: Option<String>,
    pub data_path: Option<PathBuf>,
    pub meta_path: Option<PathBuf>,
    pub samples_written: u64,
    pub bytes_written: u64,
    file: Option<File>,
    meta_owned: bool,
    origin: Option<u64>,
    next_source: Option<u64>,
    captures: Vec<Value>,
    annotations: Vec<Value>,
    gaps: Vec<Value>,
    started_at: DateTime<Utc>,
}

impl IQRecorder {
    pub fn new(output_dir: impl AsRef<Path>, sample_rate: f64, center_hz: f64,
               input_metadata: Value, max_bytes: u64, max_seconds: f64) -> Result<Self, String> {
        positive(sample_rate, "sample_rate")?; positive(max_seconds, "max_seconds")?;
        if !center_hz.is_finite() { return Err("center_hz must be finite".into()); }
        let max_bytes = max_bytes/2*2;
        if max_bytes < 2 || max_seconds*sample_rate < 1.0 { return Err("capacity must hold at least one sample".into()); }
        Ok(Self { output_dir: output_dir.as_ref().to_path_buf(), sample_rate, center_hz, input_metadata,
            max_bytes, max_seconds, active: false, limit_reached: false, last_error: None, stop_reason: None,
            data_path: None, meta_path: None, samples_written: 0, bytes_written: 0, file: None,
            meta_owned: false, origin: None, next_source: None, captures: vec![], annotations: vec![],
            gaps: vec![], started_at: Utc::now() })
    }
    pub fn duration_seconds(&self) -> f64 { self.samples_written as f64/self.sample_rate }
    pub fn summary(&self) -> Value {
        json!({"data_path":self.data_path,"meta_path":self.meta_path,"path":self.meta_path,
            "samples":self.samples_written,"bytes":self.bytes_written,"duration_seconds":self.duration_seconds(),
            "active":self.active,"limit_reached":self.limit_reached,"stop_reason":self.stop_reason,"error":self.last_error})
    }
    fn metadata(&self) -> Value {
        let state = if self.active { "recording" } else if self.last_error.is_some() || self.limit_reached { "incomplete" } else { "complete" };
        // Engine can discover several FFT events before writing their source
        // block. Resolve positions from the captures that actually reached the
        // file, rather than clamping future events to the current file length.
        let mut annotations = Vec::new();let mut unmapped_triggers=Vec::new();
        for mut annotation in self.annotations.clone(){
            let source=annotation["spectrum_studio:source_sample"].as_u64();
            if let Some((sample,boundary))=source.and_then(|source|self.annotation_position(source)){
                annotation["core:sample_start"]=json!(sample);
                annotation["spectrum_studio:position_status"]=json!(if boundary{"recorded_end_boundary"}else{"recorded_sample"});
                annotations.push(annotation);
            }else{
                let pending=self.active&&source.is_some_and(|source|self.next_source.is_none_or(|next|source>next));
                annotation["spectrum_studio:position_status"]=json!(if pending{"pending_source_samples"}else{"source_not_recorded"});
                unmapped_triggers.push(annotation);
            }
        }
        annotations.sort_by_key(|a| a["core:sample_start"].as_u64().unwrap_or(0));
        json!({"global":{
            "core:datatype":"cu8","core:version":"1.2.6","core:sample_rate":self.sample_rate,
            "core:recorder":"RTL-SDR Spectrum Studio",
            "core:description":"Unsigned interleaved I/Q; captures retain original source sample indices.",
            "core:extensions":[{"name":"spectrum_studio","version":"0.1.0","optional":true}],
            "spectrum_studio:input":self.input_metadata,
            "spectrum_studio:recording":{"state":state,"samples":self.samples_written,"bytes":self.bytes_written,
                "source_sample_start":self.origin,"source_sample_stop":self.next_source,
                "started_at_utc":self.started_at.to_rfc3339_opts(SecondsFormat::Micros,true),
                "stop_reason":self.stop_reason,"limit_reached":self.limit_reached,"error":self.last_error,
                "gaps":self.gaps,"unmapped_triggers":unmapped_triggers,"max_bytes":self.max_bytes,"max_seconds":self.max_seconds,
                "iq_normalization":"(unsigned_byte - 127.5) / 127.5"}},
            "captures":self.captures,"annotations":annotations})
    }
    pub fn start(&mut self, pre_chunks: &[(u64, Vec<u8>)]) -> Result<Value, String> {
        if self.active { return Err("IQ recording is already active".into()); }
        fs::create_dir_all(&self.output_dir).map_err(|e| e.to_string())?;
        let name = stem("iq");
        self.data_path = Some(self.output_dir.join(format!("{name}.sigmf-data")));
        self.meta_path = Some(self.output_dir.join(format!("{name}.sigmf-meta")));
        let file = OpenOptions::new().write(true).create_new(true).open(self.data_path.as_ref().unwrap()).map_err(|e| e.to_string())?;
        self.file = Some(file); self.active = true; self.meta_owned = false;
        self.samples_written = 0; self.bytes_written = 0; self.origin = None; self.next_source = None;
        self.captures.clear(); self.annotations.clear(); self.gaps.clear();
        self.limit_reached = false; self.last_error = None; self.stop_reason = None; self.started_at = Utc::now();
        if let Err(error) = atomic_json(self.meta_path.as_ref().unwrap(), &self.metadata(), true) {
            self.last_error = Some(error.clone()); self.stop("start_error"); return Err(error);
        }
        self.meta_owned = true;
        for (sample, bytes) in pre_chunks {
            if let Err(error)=self.write(bytes, *sample) {
                self.last_error=Some(error.clone());self.stop("start_error");return Err(error);
            }
            if !self.active { break; }
        }
        Ok(self.summary())
    }
    fn capture(&mut self, start: u64) {
        let stamp = self.input_metadata.get("stream_start_utc").and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .and_then(|time| time.with_timezone(&Utc).checked_add_signed(Duration::microseconds((start as f64/self.sample_rate*1e6) as i64)));
        let source = if stamp.is_some() { "source_host_clock" } else { "recording_host_estimate" };
        let time = stamp.unwrap_or(self.started_at);
        self.captures.push(json!({"core:sample_start":self.samples_written,"core:global_index":start,
            "core:frequency":self.center_hz,"core:datetime":time.to_rfc3339_opts(SecondsFormat::Micros,true),
            "spectrum_studio:timestamp_source":source}));
    }
    /// Returns accepted source samples. Caps close normally; I/O errors close and return Err.
    pub fn write(&mut self, raw: &[u8], start_sample: u64) -> Result<usize, String> {
        if raw.len()%2 != 0 { return Err("incomplete interleaved I/Q pair".into()); }
        if !self.active || raw.is_empty() { return Ok(0); }
        let mut start = start_sample; let mut data = raw;
        if let Some(next) = self.next_source {
            if start < next {
                let covered = (next-start).min((data.len()/2) as u64) as usize;
                data = &data[covered*2..]; start += covered as u64;
                if data.is_empty() { return Ok(0); }
            }
        }
        let first=self.origin.is_none();if first{self.origin=Some(start);}
        let byte_remaining = (self.max_bytes-self.bytes_written)/2;
        let time_stop = self.origin.unwrap().saturating_add((self.max_seconds*self.sample_rate) as u64);
        let time_remaining = time_stop.saturating_sub(start);
        let accepted = (data.len()/2).min(byte_remaining.min(time_remaining) as usize);
        let clipped = &data[..accepted*2];
        if !clipped.is_empty() {
            if let Err(error) = self.file.as_mut().unwrap().write_all(clipped) {
                if let Some(file) = self.file.as_mut() { let _ = file.set_len(self.bytes_written); }
                self.last_error = Some(error.to_string()); self.stop("io_error"); return Err(error.to_string());
            }
            if first{self.capture(start);}
            else if start>self.next_source.unwrap(){
                self.gaps.push(json!({"source_sample_start":self.next_source,"missing_samples":start-self.next_source.unwrap(),"file_sample_start":self.samples_written}));
                self.capture(start);
            }
            self.bytes_written += clipped.len() as u64; self.samples_written += accepted as u64;
            self.next_source = Some(start.saturating_add(accepted as u64));
        }
        let at_bytes = self.bytes_written >= self.max_bytes;
        let at_time = time_remaining <= accepted as u64;
        if accepted < data.len()/2 || at_bytes || at_time {
            self.limit_reached = true;
            self.stop(if at_bytes || byte_remaining <= time_remaining { "size_limit" } else { "duration_limit" });
        }
        Ok(accepted)
    }
    fn annotation_position(&self,source:u64)->Option<(u64,bool)>{
        for(index,capture)in self.captures.iter().enumerate(){
            let file_start=capture["core:sample_start"].as_u64()?;
            let source_start=capture["core:global_index"].as_u64()?;
            let file_end=self.captures.get(index+1).and_then(|next|next["core:sample_start"].as_u64()).unwrap_or(self.samples_written);
            let count=file_end.saturating_sub(file_start);
            if source>=source_start&&source<source_start.saturating_add(count){return Some((file_start+source-source_start,false));}
        }
        // A zero-length event may denote the exclusive saved interval boundary.
        if self.samples_written>0&&self.next_source==Some(source){Some((self.samples_written,true))}else{None}
    }
    pub fn mark_trigger(&mut self, source_sample: u64, details: Value) {
        self.annotations.push(json!({"core:sample_count":0,
            "core:label":"trigger","spectrum_studio:source_sample":source_sample,"spectrum_studio:trigger":details}));
    }
    pub fn add_trigger(&mut self, source_sample: u64, details: Value) { self.mark_trigger(source_sample, details); }
    pub fn checkpoint(&self) -> Result<(), String> {
        if self.meta_owned { atomic_json(self.meta_path.as_ref().unwrap(), &self.metadata(), false) } else { Ok(()) }
    }
    pub fn stop(&mut self, reason: &str) -> Value {
        if !self.active { return self.summary(); }
        self.active = false; self.stop_reason = Some(reason.into());
        if let Some(mut file) = self.file.take() {
            if let Err(error) = file.flush().and_then(|_| file.sync_all()) {
                if self.last_error.is_none() { self.last_error = Some(error.to_string()); }
                self.stop_reason = Some("io_error".into());
            }
        }
        if let Err(error) = self.checkpoint() { if self.last_error.is_none() { self.last_error = Some(format!("metadata: {error}")); } }
        self.summary()
    }
}
impl Drop for IQRecorder { fn drop(&mut self) { if self.active { self.stop("shutdown"); } } }

pub struct ReplayReader {
    pub meta_path: PathBuf,
    pub data_path: PathBuf,
    pub metadata: Value,
    pub sample_rate: f64,
    pub center_hz: f64,
    pub input_metadata: Value,
    pub recording_metadata: Value,
    pub captures: Vec<Value>,
    pub warnings: Vec<String>,
    pub total_samples: u64,
    pub position_samples: u64,
    file: File,
}
impl ReplayReader {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, String> {
        let mut meta_path = path.as_ref().to_path_buf();
        if meta_path.extension().and_then(|x| x.to_str()) == Some("sigmf-data") { meta_path.set_extension("sigmf-meta"); }
        let metadata: Value = serde_json::from_slice(&fs::read(&meta_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let global = metadata.get("global").ok_or("missing SigMF global metadata")?;
        if global["core:datatype"].as_str() != Some("cu8") { return Err("this version replays SigMF cu8 only".into()); }
        if global["core:metadata_only"].as_bool()==Some(true){return Err("metadata-only SigMF has no samples to replay".into());}
        if global["core:num_channels"].as_u64().unwrap_or(1)!=1{return Err("this version replays one complex IQ channel only".into());}
        if let Some(extensions)=global["core:extensions"].as_array(){
            for extension in extensions{
                let supported=extension["name"].as_str()==Some("spectrum_studio")&&extension["version"].as_str()==Some("0.1.0");
                if extension["optional"].as_bool()!=Some(true)&&!supported{return Err(format!("unsupported required SigMF extension: {}",extension["name"]));}
            }
        }
        let sample_rate = positive(global["core:sample_rate"].as_f64().ok_or("missing sample rate")?, "sample_rate")?;
        let mut data_path = meta_path.with_extension("sigmf-data");
        if let Some(dataset) = global["core:dataset"].as_str() {
            if Path::new(dataset).file_name().and_then(|p| p.to_str()) != Some(dataset) || Path::new(dataset).components().count() != 1 {
                return Err("dataset must be a filename in the metadata directory".into());
            }
            data_path = meta_path.parent().unwrap_or(Path::new(".")).join(dataset);
        }
        let file = File::open(&data_path).map_err(|e| e.to_string())?;
        let bytes = file.metadata().map_err(|e| e.to_string())?.len();
        if bytes%2 != 0 { return Err("truncated interleaved I/Q sample pair".into()); }
        let mut captures = metadata["captures"].as_array().cloned().unwrap_or_default();
        captures.sort_by_key(|c| c["core:sample_start"].as_u64().unwrap_or(0));
        if captures.is_empty() { captures.push(json!({"core:sample_start":0,"core:global_index":0})); }
        if captures[0]["core:sample_start"].as_u64() != Some(0) { return Err("first capture must start at zero".into()); }
        let center_hz = captures[0]["core:frequency"].as_f64().unwrap_or(0.0);
        if !center_hz.is_finite(){return Err("capture center frequency must be finite".into());}
        if global["core:trailing_bytes"].as_u64().unwrap_or(0)>0||captures.iter().any(|c|c["core:header_bytes"].as_u64().unwrap_or(0)>0){return Err("embedded non-conforming SigMF headers/trailers are not supported".into());}
        if captures.iter().any(|c|c["core:frequency"].as_f64().unwrap_or(center_hz)!=center_hz){return Err("frequency-changing recordings need segment-aware replay".into());}
        let input_metadata = global["spectrum_studio:input"].clone();
        let recording_metadata = global["spectrum_studio:recording"].clone();
        let mut warnings = vec![];
        if !recording_metadata.is_null() && recording_metadata["state"].as_str() != Some("complete") { warnings.push("recording was not finalized normally".into()); }
        if recording_metadata["gaps"].as_array().is_some_and(|g| !g.is_empty()) { warnings.push("source discontinuities are described in metadata".into()); }
        Ok(Self {meta_path,data_path,metadata,sample_rate,center_hz,input_metadata,recording_metadata,captures,warnings,total_samples:bytes/2,position_samples:0,file})
    }
    pub fn duration_seconds(&self) -> f64 { self.total_samples as f64/self.sample_rate }
    pub fn position_seconds(&self) -> f64 { self.position_samples as f64/self.sample_rate }
    pub fn seek_seconds(&mut self, seconds: f64) -> Result<u64, String> { nonnegative(seconds,"seconds")?; self.seek_samples((seconds*self.sample_rate) as u64) }
    pub fn seek_samples(&mut self, sample: u64) -> Result<u64, String> {
        self.position_samples = sample.min(self.total_samples);
        self.file.seek(SeekFrom::Start(self.position_samples*2)).map_err(|e| e.to_string())?;
        Ok(self.position_samples)
    }
    pub fn read_chunk(&mut self, samples: usize) -> Result<(u64, Vec<u8>), String> {
        if samples == 0 { return Err("chunk size must be positive".into()); }
        let count = samples.min((self.total_samples-self.position_samples) as usize);
        let bytes = count.checked_mul(2).ok_or("chunk size overflow")?;
        let mut data = vec![0;bytes]; let start = self.position_samples;
        self.file.read_exact(&mut data).map_err(|e| e.to_string())?;
        self.position_samples += count as u64;
        Ok((start,data))
    }
    pub fn global_sample_for(&self, sample: u64) -> u64 {
        let mut capture = &self.captures[0];
        for item in &self.captures {
            if item["core:sample_start"].as_u64().unwrap_or(0) > sample { break; }
            capture = item;
        }
        let first = capture["core:sample_start"].as_u64().unwrap_or(0);
        capture["core:global_index"].as_u64().unwrap_or(first).saturating_add(sample.saturating_sub(first))
    }
}

pub struct WavRecorder {
    output_dir: PathBuf,
    pub vfo_id: String,
    pub audio_rate: u32,
    pub format: String,
    pub max_bytes: u64,
    pub active: bool,
    pub limit_reached: bool,
    pub last_error: Option<String>,
    pub stop_reason: Option<String>,
    pub path: Option<PathBuf>,
    pub meta_path: Option<PathBuf>,
    pub samples_written: u64,
    pub bytes_written: u64,
    pub clipped_samples: u64,
    pub segment_index: u64,
    pub segments: Vec<Value>,
    pub condition_metadata: Value,
    pub processing_summary: Value,
    width: u16,
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    started_at: DateTime<Utc>,
    meta_owned: bool,
}
impl WavRecorder {
    pub fn new(output_dir: impl AsRef<Path>, vfo_id: &str, audio_rate: u32, format: &str) -> Result<Self,String> {
        if audio_rate == 0 { return Err("audio_rate must be positive".into()); }
        let format = format.to_uppercase();
        let width = match format.as_str() { "PCM16"=>2,"PCM24"=>3,_=>return Err("WAV format must be PCM16 or PCM24".into()) };
        Ok(Self {output_dir:output_dir.as_ref().to_path_buf(),vfo_id:vfo_id.into(),audio_rate,format,max_bytes:0xFFFF_FF00,
            active:false,limit_reached:false,last_error:None,stop_reason:None,path:None,meta_path:None,
            samples_written:0,bytes_written:0,clipped_samples:0,segment_index:0,segments:vec![],
            condition_metadata:json!({}),processing_summary:json!({}),width,writer:None,started_at:Utc::now(),meta_owned:false})
    }
    pub fn summary(&self) -> Value {
        json!({"path":self.path,"meta_path":self.meta_path,"vfo_id":self.vfo_id,"format":self.format,
            "audio_rate":self.audio_rate,"samples":self.samples_written,"bytes":self.bytes_written,
            "duration_seconds":self.samples_written as f64/self.audio_rate as f64,"clipped_samples":self.clipped_samples,
            "segment_index":self.segment_index,"conditions":self.condition_metadata,"active":self.active,
            "route":self.condition_metadata["wav_audio"],"processing_summary":self.processing_summary,
            "stop_reason":self.stop_reason,"error":self.last_error,"limit_reached":self.limit_reached,
            "started_at_utc":self.started_at.to_rfc3339_opts(SecondsFormat::Micros,true)})
    }
    pub fn start(&mut self, conditions: Value) -> Result<Value,String> {
        if self.active { return Err("WAV recording is already active".into()); }
        if self.max_bytes < self.width as u64 { return Err("WAV capacity must hold one sample".into()); }
        fs::create_dir_all(&self.output_dir).map_err(|e| e.to_string())?;
        let name = stem(&format!("vfo_{}",self.vfo_id));
        self.path = Some(self.output_dir.join(format!("{name}.wav")));
        self.meta_path = Some(self.output_dir.join(format!("{name}.wav.json")));
        let file = OpenOptions::new().write(true).create_new(true).open(self.path.as_ref().unwrap()).map_err(|e| e.to_string())?;
        let spec = hound::WavSpec {channels:1,sample_rate:self.audio_rate,bits_per_sample:self.width*8,sample_format:hound::SampleFormat::Int};
        self.writer = Some(hound::WavWriter::new(BufWriter::new(file),spec).map_err(|e| e.to_string())?);
        self.condition_metadata = conditions; self.active = true; self.meta_owned = false;
        self.processing_summary=json!({"observation_scope":"DSP blocks presented to this WAV segment; the final capacity/error block may contain more observed than accepted audio samples",
            "afc_observation":"Correction at the end of each presented DSP block; min/max exclude within-block extrema",
            "rf_observation":"Filtered channel mean power for each presented DSP block; uncalibrated dBFS"});
        self.samples_written = 0; self.bytes_written = 0; self.clipped_samples = 0;
        self.last_error = None; self.stop_reason = None; self.limit_reached = false; self.started_at = Utc::now();
        if let Err(error) = atomic_json(self.meta_path.as_ref().unwrap(),&self.summary(),true) {
            self.last_error=Some(error.clone());self.stop("start_error");return Err(error);
        }
        self.meta_owned = true;
        Ok(self.summary())
    }
    /// Counters describe the DSP blocks presented to this segment, separately
    /// from samples_written, which is the actual accepted PCM sample count.
    pub fn observe_processing(&mut self, observation:&Value) {
        if !self.active{return;}
        for key in ["observed_blocks","source_complex_samples","observed_audio_samples","raw_adc_rail_components",
            "demod_overrange_samples","pre_overload_samples","limited_samples","muted_samples","invalid_samples","boundary_repairs","gap_count"] {
            if let Some(value)=observation[key].as_u64(){let old=self.processing_summary[key].as_u64().unwrap_or(0);
                self.processing_summary[key]=json!(old.saturating_add(value));}
        }
        for key in ["afc_offset_hz","rf_dbfs"] {
            if let Some(value)=observation[key].as_f64().filter(|v|v.is_finite()) {
                let low=format!("{key}_min");let high=format!("{key}_max");
                let minimum=self.processing_summary[&low].as_f64().unwrap_or(value).min(value);
                let maximum=self.processing_summary[&high].as_f64().unwrap_or(value).max(value);
                self.processing_summary[&low]=json!(minimum);self.processing_summary[&high]=json!(maximum);
                self.processing_summary[key]=json!(value);
            }
        }
    }
    pub fn write(&mut self, audio: &[f32]) -> Result<usize,String> {
        if !self.active || audio.is_empty() { return Ok(0); }
        if audio.iter().any(|v| !v.is_finite()) {
            let error="non-finite audio samples".to_string(); self.last_error=Some(error.clone());self.stop("invalid_audio");return Err(error);
        }
        let remaining = self.max_bytes.saturating_sub(self.bytes_written)/self.width as u64;
        let accepted = audio.len().min(remaining as usize);
        let limit = (1_i64 << (self.width*8-1)) as f64;
        for &value in &audio[..accepted] {
            let value = value as f64;
            if value < -1.0 || value > (limit-1.0)/limit { self.clipped_samples += 1; }
            let sample = (value.clamp(-1.0,(limit-1.0)/limit)*limit).round() as i32;
            let result = if self.width==2 {self.writer.as_mut().unwrap().write_sample(sample as i16)}
                         else {self.writer.as_mut().unwrap().write_sample(sample)};
            if let Err(error) = result {
                self.last_error=Some(error.to_string()); self.stop("io_error");return Err(error.to_string());
            }
            self.samples_written += 1; self.bytes_written += self.width as u64;
        }
        if accepted<audio.len() || self.bytes_written+self.width as u64>self.max_bytes {
            self.limit_reached=true;self.stop("size_limit");
        }
        Ok(accepted)
    }
    pub fn stop(&mut self, reason: &str) -> Value {
        if !self.active { return self.summary(); }
        self.active=false;self.stop_reason=Some(reason.into());
        if let Some(writer)=self.writer.take() {
            if let Err(error)=writer.finalize() {
                if self.last_error.is_none(){self.last_error=Some(error.to_string());}
                self.stop_reason=Some("io_error".into());
            }
        }
        if self.meta_owned {
            if let Err(error)=atomic_json(self.meta_path.as_ref().unwrap(),&self.summary(),false) {
                if self.last_error.is_none(){self.last_error=Some(format!("metadata: {error}"));}
            }
        }
        let summary=self.summary();self.segments.push(summary.clone());summary
    }
    pub fn split(&mut self, conditions: Value) -> Result<Value,String> {
        let was_active=self.active;
        let previous=if was_active {self.stop("conditions_changed")} else {Value::Null};
        self.condition_metadata=conditions.clone();self.segment_index+=1;
        let current=if was_active {self.start(conditions)?} else {self.summary()};
        Ok(json!({"previous":previous,"current":current}))
    }
}
impl Drop for WavRecorder { fn drop(&mut self) {if self.active {self.stop("shutdown");}} }

#[cfg(test)]
mod tests {
    use super::*;
    fn dir() -> PathBuf {std::env::temp_dir().join(stem("spectrum_recording_test"))}
    fn raw(samples: usize,value:u8)->Vec<u8>{vec![value;samples*2]}
    #[test]
    fn bounded_prebuffer_and_trigger_boundary(){
        let mut b=PreTriggerBuffer::new(10.0,20).unwrap();
        b.append(0,&raw(8,1)).unwrap();b.append(8,&raw(8,2)).unwrap();
        let (chunks,seconds)=b.get(0.6,Some(12)).unwrap();
        assert_eq!(chunks,vec![(6,raw(2,1)),(8,raw(4,2))]);assert!((seconds-0.6).abs()<1e-9);
        b.append(20,&raw(2,3)).unwrap();assert_eq!(b.gap_count,1);assert_eq!(b.available_seconds(),0.2);
    }
    #[test]
    fn iq_pre_roll_overlap_gap_replay_and_caps(){
        let path=dir();let mut r=IQRecorder::new(&path,10.0,32e6,json!({"gain_db":20.7}),1000,120.0).unwrap();
        r.start(&[(100,raw(5,1))]).unwrap();assert_eq!(r.write(&raw(8,2),102).unwrap(),5);
        r.write(&raw(3,3),120).unwrap();r.mark_trigger(104,json!({"metric":"peak"}));
        let summary=r.stop("manual");assert_eq!(summary["samples"],13);
        let mut p=ReplayReader::new(r.meta_path.as_ref().unwrap()).unwrap();
        assert_eq!(p.global_sample_for(11),121);p.seek_seconds(0.5).unwrap();assert_eq!(p.read_chunk(4).unwrap(),(5,raw(4,2)));
        assert_eq!(p.seek_seconds(100.0).unwrap(),13);assert!(p.read_chunk(1).unwrap().1.is_empty());
        let mut limited=IQRecorder::new(&path,10.0,1e6,json!({}),10,1.0).unwrap();limited.start(&[]).unwrap();
        assert_eq!(limited.write(&raw(10,128),0).unwrap(),5);assert!(!limited.active&&limited.limit_reached);
        assert_eq!(limited.stop_reason.as_deref(),Some("size_limit"));
    }
    #[test]
    fn duration_cap_includes_source_gaps(){
        let path=dir();let mut r=IQRecorder::new(&path,10.0,1e6,json!({}),1000,1.0).unwrap();r.start(&[]).unwrap();
        r.write(&raw(2,1),0).unwrap();assert_eq!(r.write(&raw(10,2),8).unwrap(),2);
        assert_eq!(r.samples_written,4);assert_eq!(r.stop_reason.as_deref(),Some("duration_limit"));
    }
    #[test]
    fn future_retrigger_positions_resolve_after_block_write_with_preroll(){
        let mut r=IQRecorder::new(dir(),1000.0,32e6,json!({}),10000,120.0).unwrap();r.start(&[(1000,raw(100,1))]).unwrap();
        for source in [1100,1150,1190]{r.mark_trigger(source,json!({"type":if source==1100{"trigger"}else{"extend"}}));}
        r.checkpoint().unwrap();let pending:Value=serde_json::from_slice(&fs::read(r.meta_path.as_ref().unwrap()).unwrap()).unwrap();
        assert_eq!(pending["global"]["spectrum_studio:recording"]["unmapped_triggers"].as_array().unwrap().len(),2);
        assert_eq!(pending["global"]["spectrum_studio:recording"]["unmapped_triggers"][0]["spectrum_studio:position_status"],"pending_source_samples");
        r.write(&raw(100,2),1100).unwrap();r.stop("post_trigger_complete");
        let metadata:Value=serde_json::from_slice(&fs::read(r.meta_path.as_ref().unwrap()).unwrap()).unwrap();
        let positions:Vec<u64>=metadata["annotations"].as_array().unwrap().iter().map(|event|event["core:sample_start"].as_u64().unwrap()).collect();
        assert_eq!(positions,vec![100,150,190]);
        assert_eq!(metadata["annotations"][1]["spectrum_studio:source_sample"],1150);
        assert!(metadata["global"]["spectrum_studio:recording"]["unmapped_triggers"].as_array().unwrap().is_empty());
    }
    #[test]
    fn annotation_mapping_preserves_gaps_and_reports_events_excluded_by_caps(){
        let mut r=IQRecorder::new(dir(),1000.0,32e6,json!({}),10000,120.0).unwrap();r.start(&[(1000,raw(100,1))]).unwrap();
        r.mark_trigger(1505,json!({"type":"extend"}));r.mark_trigger(1200,json!({"type":"gap_event"}));
        r.write(&raw(20,2),1500).unwrap();r.stop("manual");let metadata=r.metadata();
        assert_eq!(metadata["annotations"].as_array().unwrap().len(),1);assert_eq!(metadata["annotations"][0]["core:sample_start"],105);
        assert_eq!(metadata["global"]["spectrum_studio:recording"]["gaps"][0]["missing_samples"],400);
        let missing=&metadata["global"]["spectrum_studio:recording"]["unmapped_triggers"][0];
        assert_eq!(missing["spectrum_studio:source_sample"],1200);assert_eq!(missing["spectrum_studio:position_status"],"source_not_recorded");assert!(missing.get("core:sample_start").is_none());
        let mut capped=IQRecorder::new(dir(),1000.0,32e6,json!({}),20,120.0).unwrap();capped.start(&[]).unwrap();
        capped.mark_trigger(0,json!({"type":"trigger"}));capped.mark_trigger(12,json!({"type":"extend"}));
        capped.write(&raw(5,1),0).unwrap();capped.write(&raw(10,2),5).unwrap();let metadata=capped.metadata();
        assert!(capped.limit_reached);assert_eq!(capped.samples_written,10);assert_eq!(metadata["annotations"][0]["core:sample_start"],0);
        assert_eq!(metadata["global"]["spectrum_studio:recording"]["unmapped_triggers"][0]["spectrum_studio:source_sample"],12);
        assert_eq!(metadata["annotations"].as_array().unwrap().len(),1);
    }
    #[test]
    fn duration_cap_does_not_add_a_capture_for_an_entirely_unwritten_gap_block(){
        let mut r=IQRecorder::new(dir(),10.0,32e6,json!({}),1000,1.0).unwrap();r.start(&[]).unwrap();r.write(&raw(2,1),0).unwrap();
        r.mark_trigger(100,json!({"type":"extend"}));assert_eq!(r.write(&raw(3,2),100).unwrap(),0);let metadata=r.metadata();
        assert!(r.limit_reached);assert_eq!(metadata["captures"].as_array().unwrap().len(),1);
        assert_eq!(metadata["global"]["spectrum_studio:recording"]["unmapped_triggers"][0]["spectrum_studio:source_sample"],100);
    }
    #[test]
    fn wav_formats_split_and_limits(){
        let path=dir();
        for format in ["PCM16","PCM24"] {
            let mut r=WavRecorder::new(&path,format,32000,format).unwrap();r.start(json!({"mode":"AM"})).unwrap();
            r.write(&[-1.0,-0.5,0.0,0.5,1.0]).unwrap();let old=r.path.clone().unwrap();r.split(json!({"mode":"NFM"})).unwrap();
            assert_ne!(r.path.as_ref().unwrap(),&old);assert!(r.active);r.stop("manual");
            let mut read=hound::WavReader::open(old).unwrap();assert_eq!(read.duration(),5);
            let samples:Vec<i32>=read.samples::<i32>().map(Result::unwrap).collect();
            let limit=if format=="PCM16"{32768}else{8388608};assert_eq!(samples,vec![-limit,-limit/2,0,limit/2,limit-1]);
        }
        let mut r=WavRecorder::new(&path,"limited",32000,"PCM16").unwrap();r.max_bytes=6;r.start(json!({})).unwrap();
        assert_eq!(r.write(&[0.0;10]).unwrap(),3);assert!(!r.active&&r.limit_reached);
    }
    #[test]
    fn wav_processing_observations_are_segment_scoped_and_separate_from_accepted_samples(){
        let path=dir();let mut r=WavRecorder::new(&path,"processing",32000,"PCM16").unwrap();
        r.start(json!({"wav_audio":"demod","reception":{"audio_gain_db":0.0}})).unwrap();
        r.observe_processing(&json!({"observed_blocks":1,"observed_audio_samples":2,"pre_overload_samples":1,"limited_samples":1,"afc_offset_hz":12.0,"rf_dbfs":-50.0}));
        r.write(&[0.1,-0.1]).unwrap();let first_meta=r.meta_path.clone().unwrap();
        r.split(json!({"wav_audio":"processed","reception":{"audio_gain_db":6.0}})).unwrap();
        let first:Value=serde_json::from_slice(&fs::read(first_meta).unwrap()).unwrap();
        assert_eq!(first["samples"],2);assert_eq!(first["processing_summary"]["pre_overload_samples"],1);assert_eq!(first["route"],"demod");
        assert!(r.processing_summary.get("observed_audio_samples").is_none());assert_eq!(r.summary()["route"],"processed");
        r.max_bytes=6;r.observe_processing(&json!({"observed_blocks":1,"observed_audio_samples":10,"pre_overload_samples":8,"limited_samples":8,"afc_offset_hz":-5.0,"rf_dbfs":-40.0}));
        assert_eq!(r.write(&[0.25;10]).unwrap(),3);assert!(!r.active);assert_eq!(r.samples_written,3);
        let last:Value=serde_json::from_slice(&fs::read(r.meta_path.as_ref().unwrap()).unwrap()).unwrap();
        assert_eq!(last["processing_summary"]["observed_audio_samples"],10);assert_eq!(last["processing_summary"]["pre_overload_samples"],8);
        assert_eq!(last["processing_summary"]["afc_offset_hz_min"],-5.0);assert_eq!(last["processing_summary"]["afc_offset_hz_max"],-5.0);
        assert!(last["processing_summary"]["observation_scope"].as_str().unwrap().contains("more observed than accepted"));
    }
    #[test]
    fn invalid_capacity_and_destination_are_rejected(){
        assert!(PreTriggerBuffer::new(10.0,1).is_err());
        assert!(IQRecorder::new(dir(),0.0,1e6,json!({}),100,1.0).is_err());
        let path=dir();fs::create_dir_all(&path).unwrap();let blocker=path.join("not_a_directory");fs::write(&blocker,b"preserve").unwrap();
        let mut r=IQRecorder::new(&blocker,10.0,1e6,json!({}),100,1.0).unwrap();assert!(r.start(&[]).is_err());assert!(!r.active);
    }
}
