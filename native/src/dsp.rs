//! Streaming DSP. Complex magnitude one is power one (0 dBFS).
//! FFT PSD is two-sided power/Hz. Drawn traces use PSD × window ENBW;
//! measurement power integrates the original PSD × frequency-bin width.
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::f64::consts::{LN_2, PI, TAU};
use std::sync::Arc;

fn db(power: f64) -> f64 { 10.0 * power.max(1e-30).log10() }

/// Fixed output headroom, not AGC or an S/N improvement. Nominal WFM
/// ±75 kHz deviation maps to ±0.5 before audio filtering/de-emphasis.
/// AM carrier-normalized envelope and NFM nominal-deviation units retain
/// gain one. Floating output is not limited; the PCM sink counts clipping.
pub fn demod_audio_scale(mode: &str) -> f64 {
    if mode.eq_ignore_ascii_case("WFM") { 0.5 } else { 1.0 }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sample_rate: f64,
    pub fft_size: usize,
    pub window: String,
    pub overlap: f64,
    pub ref_level: f64,
    pub range_db: f64,
    pub persistence_mode: String,
    pub persistence_seconds: f64,
    pub persistence_frames: usize,
    pub persistence_unit: String,
    pub switch_policy: String,
    pub stats_mode: String,
    pub density_metric: String,
    pub detector: String,
    pub average_seconds: f64,
    pub waterfall_row_seconds: f64,
    pub density_min_db: f64,
    pub density_max_db: f64,
    pub density_levels: usize,
    pub max_density_history_mb: f64,
    pub waterfall_rows: usize,
}
pub type AnalysisConfig = Config;

impl Default for Config {
    fn default() -> Self {
        Self { sample_rate: 2_048_000.0, fft_size: 4096, window: "hann".into(), overlap: 0.5,
            ref_level: -20.0, range_db: 100.0, persistence_mode: "exponential".into(),
            persistence_seconds: 1.0, persistence_frames: 1000, persistence_unit: "half_life".into(),
            switch_policy: "convert".into(), stats_mode: "persistence".into(), density_metric: "rate".into(),
            detector: "positive_peak".into(), average_seconds: 1.0, waterfall_row_seconds: 0.1,
            density_min_db: -140.0, density_max_db: 0.0, density_levels: 128,
            max_density_history_mb: 64.0, waterfall_rows: 250 }
    }
}

pub fn validate_config(c: &Config) -> Result<(), String> {
    if !c.sample_rate.is_finite() || c.sample_rate <= 0.0 { return Err("サンプルレートは正の値が必要です".into()); }
    if !(128..=32768).contains(&c.fft_size) || !c.fft_size.is_power_of_two() { return Err("FFT点数は128〜32768の2の累乗です".into()); }
    if !c.overlap.is_finite() || !(0.0..0.95).contains(&c.overlap) { return Err("FFT重なりは0以上0.95未満です".into()); }
    if !["rect", "rectangular", "boxcar", "none", "hann", "hanning", "hamming", "blackman"].contains(&c.window.as_str()) { return Err("未対応の窓関数です".into()); }
    if !["off", "infinite", "exponential", "window", "time_window"].contains(&c.persistence_mode.as_str()) { return Err("Persistenceモードが不正です".into()); }
    if !["half_life", "time_constant", "frames"].contains(&c.persistence_unit.as_str()) { return Err("Persistence指定単位が不正です".into()); }
    if !["convert", "retain", "clear"].contains(&c.switch_policy.as_str()) { return Err("Persistence切替方式が不正です".into()); }
    if !["cumulative", "persistence"].contains(&c.stats_mode.as_str()) { return Err("統計方式が不正です".into()); }
    if !["rate", "hits", "count"].contains(&c.density_metric.as_str()) { return Err("密度単位が不正です".into()); }
    if !["sample", "positive_peak", "negative_peak", "rms"].contains(&c.detector.as_str()) { return Err("表示検波が不正です".into()); }
    for x in [c.range_db, c.persistence_seconds, c.average_seconds, c.waterfall_row_seconds, c.max_density_history_mb] {
        if !x.is_finite() || x <= 0.0 { return Err("時間・範囲・メモリー上限は正の値が必要です".into()); }
    }
    if !c.ref_level.is_finite() || !c.density_min_db.is_finite() || !c.density_max_db.is_finite()
        || c.density_max_db <= c.density_min_db || !(16..=256).contains(&c.density_levels)
        || c.persistence_frames == 0 || !(1..=1000).contains(&c.waterfall_rows) {
        return Err("密度または表示範囲が不正です".into());
    }
    Ok(())
}

fn window_values(name: &str, n: usize) -> Vec<f64> {
    (0..n).map(|k| {
        let x = TAU * k as f64 / n as f64;
        match name {
            "hann" | "hanning" => 0.5 - 0.5*x.cos(),
            "hamming" => 0.54 - 0.46*x.cos(),
            "blackman" => 0.42 - 0.5*x.cos() + 0.08*(2.0*x).cos(),
            _ => 1.0,
        }
    }).collect()
}

pub fn fft_size_for_rbw(fs: f64, target: f64, window: &str) -> usize {
    if !fs.is_finite() || !target.is_finite() || fs <= 0.0 || target <= 0.0 { return 4096; }
    (7..=15).map(|p| 1usize << p).min_by(|a,b| {
        let error = |n: usize| { let w = window_values(window,n); let sum: f64 = w.iter().sum();
            let bw = fs*w.iter().map(|v|v*v).sum::<f64>()/(sum*sum); (bw/target).ln().abs() };
        error(*a).total_cmp(&error(*b))
    }).unwrap_or(4096)
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessResult {
    pub frames_processed: usize,
    pub last_psd: Vec<f64>,
    pub fft_frame_psd: Vec<Vec<f64>>,
    pub fft_frame_end_samples: Vec<u64>,
    pub frame_end_samples: Vec<u64>,
    pub peak_dbfs: f64,
    pub integrated_dbfs: f64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub gap_samples: u64,
}

struct WindowEntry { time: f64, levels: Vec<u8> }

pub struct Analyzer {
    pub config: Config,
    pub hop: usize,
    pub bin_hz: f64,
    pub enbw_hz: f64,
    pub frequency_offsets_hz: Vec<f64>,
    pub total_valid_frames: u64,
    pub invalid_frames: u64,
    pub gap_samples: u64,
    pub last_psd: Vec<f64>,
    fft: Arc<dyn Fft<f32>>,
    fft_scratch: Vec<Complex32>,
    fft_buffer: Vec<Complex32>,
    window: Vec<f64>,
    psd_scale: f64,
    tail: Vec<Complex32>,
    tail_start: Option<u64>,
    expected_next: Option<u64>,
    max_psd: Vec<f64>,
    min_psd: Vec<f64>,
    avg_acc: Vec<f64>,
    avg_weight: f64,
    cumulative: Vec<u64>,
    exponential: Vec<f64>,
    window_hist: Vec<u32>,
    window_entries: VecDeque<WindowEntry>,
    window_bytes: usize,
    window_truncated: bool,
    window_last_forced_drop: Option<(f64,u64)>,
    last_levels: Option<Vec<u8>>,
    exp_denominator: f64,
    last_frame_time: Option<f64>,
    waterfall: VecDeque<Vec<f32>>,
    waterfall_times: VecDeque<Option<f64>>,
    waterfall_bucket: Option<i64>,
    waterfall_sum: Vec<f64>,
    waterfall_count: usize,
    unit_values: HashMap<String,(f64,usize)>,
}

impl Analyzer {
    pub fn new(config: Config) -> Self {
        validate_config(&config).expect("Analyzer config must be validated");
        let n = config.fft_size;
        let levels = config.density_levels;
        let window = window_values(&config.window,n);
        let sum: f64 = window.iter().sum();
        let sum2: f64 = window.iter().map(|v|v*v).sum();
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let scratch_len = fft.get_inplace_scratch_len();
        let fs = config.sample_rate;
        Self { hop: ((n as f64*(1.0-config.overlap)).round() as usize).max(1),
            bin_hz: fs/n as f64, enbw_hz: fs*sum2/(sum*sum),
            frequency_offsets_hz: (0..n).map(|i|(i as f64-n as f64/2.0)*fs/n as f64).collect(),
            total_valid_frames:0, invalid_frames:0, gap_samples:0, last_psd:vec![0.0;n],
            fft, fft_scratch:vec![Complex32::new(0.0,0.0);scratch_len],
            fft_buffer:vec![Complex32::new(0.0,0.0);n], window, psd_scale:fs*sum2,
            tail:Vec::new(), tail_start:None, expected_next:None,
            max_psd:vec![0.0;n], min_psd:vec![f64::INFINITY;n], avg_acc:vec![0.0;n], avg_weight:0.0,
            cumulative:vec![0;n*levels], exponential:vec![0.0;n*levels], window_hist:vec![0;n*levels],
            window_entries:VecDeque::new(), window_bytes:0, window_truncated:false, window_last_forced_drop:None, last_levels:None,
            exp_denominator:0.0, last_frame_time:None, waterfall:VecDeque::new(), waterfall_times:VecDeque::new(),
            waterfall_bucket:None, waterfall_sum:vec![0.0;n], waterfall_count:0, unit_values:HashMap::new(), config }
    }

    pub fn reset(&mut self, config: Option<Config>) { *self = Self::new(config.unwrap_or_else(||self.config.clone())); }

    pub fn average_psd(&self) -> Vec<f64> {
        let divisor = self.avg_weight.max(1e-30);
        self.avg_acc.iter().map(|p|p/divisor).collect()
    }

    /// Restart only Max/Min/Average from the latest valid Live frame. The
    /// sample clock, partially assembled FFT, density and waterfall continue.
    pub fn reset_traces(&mut self) {
        if self.last_frame_time.is_some() {
            self.max_psd.clone_from(&self.last_psd);
            self.min_psd.clone_from(&self.last_psd);
            self.avg_acc.clone_from(&self.last_psd);
            self.avg_weight=1.0;
        } else {
            self.max_psd.fill(0.0);
            // Keep the normal first-frame sentinel internally; snapshot's
            // finite guard renders the empty trace at the dB floor.
            self.min_psd.fill(f64::INFINITY);
            self.avg_acc.fill(0.0);
            self.avg_weight=0.0;
        }
    }

    fn tau(&self) -> f64 {
        match self.config.persistence_unit.as_str() {
            "frames" => self.config.persistence_frames as f64*self.hop as f64/self.config.sample_rate/LN_2,
            "time_constant" => self.config.persistence_seconds,
            _ => self.config.persistence_seconds/LN_2,
        }
    }

    fn clear_density(&mut self) {
        self.cumulative.fill(0); self.exponential.fill(0.0); self.window_hist.fill(0);
        self.total_valid_frames=0; self.exp_denominator=0.0; self.last_levels=None;
        self.window_entries.clear(); self.window_bytes=0; self.window_truncated=false; self.window_last_forced_drop=None;
    }

    pub fn update_config(&mut self, mut config: Config) -> Result<Value,String> {
        validate_config(&config)?;
        let old = &self.config;
        if old.sample_rate!=config.sample_rate || old.fft_size!=config.fft_size || old.window!=config.window
            || old.overlap!=config.overlap || old.density_levels!=config.density_levels
            || old.density_min_db!=config.density_min_db || old.density_max_db!=config.density_max_db {
            self.reset(Some(config));
            return Ok(json!({"history_cleared":true,"reason":"analysis_conditions_changed"}));
        }
        let switched = old.persistence_unit!=config.persistence_unit;
        self.unit_values.insert(old.persistence_unit.clone(),(old.persistence_seconds,old.persistence_frames));
        if switched && config.switch_policy=="convert" {
            let tau = self.tau();
            match config.persistence_unit.as_str() {
                "time_constant" => config.persistence_seconds=tau,
                "half_life" => config.persistence_seconds=tau*LN_2,
                _ => config.persistence_frames=((tau*LN_2*config.sample_rate/self.hop as f64).round() as usize).max(1),
            }
        } else if switched {
            if let Some((seconds,frames))=self.unit_values.get(&config.persistence_unit) {
                config.persistence_seconds=*seconds; config.persistence_frames=*frames;
            }
        }
        let clear=(switched && config.switch_policy!="convert") || config.persistence_mode!=old.persistence_mode;
        self.config=config;
        if clear { self.clear_density(); } else { self.prune_window_history(); }
        Ok(json!({"history_cleared":clear,"reason":if clear {Some("persistence_setting_changed")} else {None}}))
    }

    pub fn note_gap(&mut self, samples: u64) {
        self.gap_samples=self.gap_samples.saturating_add(samples); self.tail.clear(); self.tail_start=None; self.expected_next=None;
        self.push_waterfall(vec![f32::NAN;self.config.fft_size],None);
        self.waterfall_bucket=None; self.waterfall_sum.fill(0.0); self.waterfall_count=0;
    }

    fn push_waterfall(&mut self,row:Vec<f32>,time:Option<f64>) {
        self.waterfall.push_back(row); self.waterfall_times.push_back(time);
        while self.waterfall.len()>self.config.waterfall_rows {self.waterfall.pop_front();self.waterfall_times.pop_front();}
    }

    fn prune_window_history(&mut self) {
        // Expire the requested time/frame window before applying the memory
        // cap. Ordinary expiry must not look like a shortened requested window.
        self.window_truncated=false;
        let Some(latest)=self.window_entries.back().map(|entry|entry.time).or(self.last_frame_time) else{return;};
        let cap=(self.config.max_density_history_mb*1024.0*1024.0) as usize;
        let levels_count=self.config.density_levels;
        loop {
            let expired=match self.window_entries.front() {
                Some(front)=>if self.config.persistence_unit=="frames"{self.window_entries.len()>self.config.persistence_frames}
                    else{front.time<=latest-self.config.persistence_seconds+1e-12},
                None=>false,
            };
            if self.window_entries.is_empty() || (!expired&&self.window_bytes<=cap){break;}
            if !expired&&self.window_bytes>cap{
                self.window_last_forced_drop=Some((self.window_entries.front().unwrap().time,
                    self.total_valid_frames.saturating_sub(self.window_entries.len() as u64)+1));
            }
            let old=self.window_entries.pop_front().unwrap();self.window_bytes-=old.levels.len();
            for(k,level)in old.levels.into_iter().enumerate(){
                let cell=k*levels_count+level as usize;
                self.window_hist[cell]=self.window_hist[cell].saturating_sub(1);
            }
        }
        if let Some((time,sequence))=self.window_last_forced_drop {
            self.window_truncated=if self.config.persistence_unit=="frames"{
                self.total_valid_frames.saturating_sub(sequence)<self.config.persistence_frames as u64
            }else{time>latest-self.config.persistence_seconds+1e-12};
        }
    }

    pub fn process(&mut self, iq:&[Complex32], start_sample:u64) -> ProcessResult {
        if let Some(expected)=self.expected_next {if expected!=start_sample {self.note_gap(start_sample.saturating_sub(expected));}}
        self.expected_next=Some(start_sample.saturating_add(iq.len() as u64));
        let first=*self.tail_start.get_or_insert(start_sample);
        self.tail.extend_from_slice(iq);
        let n=self.config.fft_size;
        let count=if self.tail.len()>=n {(self.tail.len()-n)/self.hop+1} else {0};
        let mut frames=Vec::with_capacity(count);
        let mut ends=Vec::with_capacity(count);
        for f in 0..count {
            let begin=f*self.hop;
            let input=&self.tail[begin..begin+n];
            if input.iter().any(|v|!v.re.is_finite() || !v.im.is_finite()) {self.invalid_frames+=1;continue;}
            for k in 0..n {self.fft_buffer[k]=input[k]*(self.window[k] as f32);}
            self.fft.process_with_scratch(&mut self.fft_buffer,&mut self.fft_scratch);
            let mut psd=vec![0.0;n];
            for k in 0..n {let value=self.fft_buffer[(k+n/2)%n];psd[k]=(value.re as f64*value.re as f64+value.im as f64*value.im as f64)/self.psd_scale;}
            frames.push(psd); ends.push(first+(begin+n) as u64);
        }
        if count>0 {let consumed=count*self.hop;self.tail.drain(..consumed);self.tail_start=Some(first+consumed as u64);}
        if !frames.is_empty() {
            let latest=*ends.last().unwrap() as f64/self.config.sample_rate;
            let elapsed=self.last_frame_time.map(|t|(latest-t).max(0.0)).unwrap_or(0.0);
            let avg_decay=(-elapsed/self.config.average_seconds).exp();
            let decay=(-elapsed/self.tau()).exp();
            for p in &mut self.avg_acc {*p*=avg_decay;}
            self.avg_weight*=avg_decay;
            for h in &mut self.exponential {*h*=decay;}
            self.exp_denominator*=decay;
            let tau=self.tau();
            let levels_count=self.config.density_levels;
            let max_level=levels_count-1;
            for (frame,end) in frames.iter().zip(ends.iter()) {
                let time=*end as f64/self.config.sample_rate;
                let avg_weight=(-(latest-time)/self.config.average_seconds).exp();
                let weight=(-(latest-time)/tau).exp();
                self.avg_weight+=avg_weight; self.exp_denominator+=weight;
                let mut row=vec![0u8;n];
                for k in 0..n {
                    let p=frame[k];
                    self.max_psd[k]=self.max_psd[k].max(p);self.min_psd[k]=self.min_psd[k].min(p);
                    self.avg_acc[k]+=p*avg_weight;
                    let level=(((db(p*self.enbw_hz)-self.config.density_min_db)*levels_count as f64
                        /(self.config.density_max_db-self.config.density_min_db)).floor().max(0.0) as usize).min(max_level);
                    row[k]=level as u8;
                    let cell=k*levels_count+level;
                    self.cumulative[cell]=self.cumulative[cell].saturating_add(1);
                    self.exponential[cell]+=weight;self.window_hist[cell]=self.window_hist[cell].saturating_add(1);
                }
                self.window_bytes+=row.len();
                self.window_entries.push_back(WindowEntry{time,levels:row});
                let bucket=(time/self.config.waterfall_row_seconds).floor() as i64;
                if self.waterfall_bucket.is_some() && self.waterfall_bucket!=Some(bucket) {
                    if self.waterfall_count>0 {
                        let row=self.waterfall_sum.iter().map(|p|(p/self.waterfall_count as f64) as f32).collect();
                        self.push_waterfall(row,Some((self.waterfall_bucket.unwrap() as f64+0.5)*self.config.waterfall_row_seconds));
                    }
                    self.waterfall_sum.fill(0.0);self.waterfall_count=0;
                }
                self.waterfall_bucket=Some(bucket);
                for k in 0..n {self.waterfall_sum[k]+=frame[k];}
                self.waterfall_count+=1;
            }
            self.total_valid_frames=self.total_valid_frames.saturating_add(frames.len() as u64);
            self.last_frame_time=Some(latest);
            self.prune_window_history();
            self.last_levels=self.window_entries.back().map(|e|e.levels.clone());
            // Even a zero-history memory cap must retain the last instantaneous frame.
            if self.last_levels.is_none() {
                let p=frames.last().unwrap();
                self.last_levels=Some(p.iter().map(|v|(((db(v*self.enbw_hz)-self.config.density_min_db)
                    *levels_count as f64/(self.config.density_max_db-self.config.density_min_db)).floor().max(0.0) as usize).min(max_level) as u8).collect());
            }
            self.last_psd=frames.last().unwrap().clone();
        }
        ProcessResult { frames_processed:frames.len(),last_psd:self.last_psd.clone(),
            fft_frame_psd:frames,frame_end_samples:ends.clone(),fft_frame_end_samples:ends,
            peak_dbfs:db(self.last_psd.iter().copied().fold(0.0,f64::max)*self.enbw_hz),
            integrated_dbfs:db(self.last_psd.iter().sum::<f64>()*self.bin_hz),
            start_sample,end_sample:start_sample.saturating_add(iq.len() as u64),gap_samples:self.gap_samples }
    }

    fn persistence_denominator(&self) -> f64 {
        match self.config.persistence_mode.as_str() {
            "infinite" => self.total_valid_frames as f64,
            "window"|"time_window" => self.window_entries.len() as f64,
            "off" => if self.last_levels.is_some(){1.0}else{0.0},
            _ => self.exp_denominator,
        }
    }
    fn persistence_cell(&self,k:usize,level:usize) -> f64 {
        let cell=k*self.config.density_levels+level;
        match self.config.persistence_mode.as_str() {
            "infinite"=>self.cumulative[cell] as f64,
            "window"|"time_window"=>self.window_hist[cell] as f64,
            "off"=>if self.last_levels.as_ref().map(|row|row[k] as usize==level).unwrap_or(false){1.0}else{0.0},
            _=>self.exponential[cell],
        }
    }

    pub fn density_statistics(&self) -> (Vec<f64>,f64) {
        if self.config.stats_mode=="cumulative" {return (self.cumulative.iter().map(|v|*v as f64).collect(),self.total_valid_frames as f64);}
        let mut values=vec![0.0;self.config.fft_size*self.config.density_levels];
        for k in 0..self.config.fft_size {for l in 0..self.config.density_levels {values[k*self.config.density_levels+l]=self.persistence_cell(k,l);}}
        (values,self.persistence_denominator())
    }

    pub fn snapshot(&self,span_hz:f64,center_hz:f64,width:usize) -> Value {
        let span=if span_hz.is_finite() && span_hz>0.0 {span_hz.min(self.config.sample_rate)}else{self.config.sample_rate};
        let mut selected:Vec<usize>=self.frequency_offsets_hz.iter().enumerate().filter(|(_,f)|f.abs()<=span/2.0).map(|(i,_)|i).collect();
        if selected.is_empty(){selected.push(self.config.fft_size/2);}
        let count=width.max(1).min(selected.len());
        let groups:Vec<&[usize]>=(0..count).map(|i|&selected[i*selected.len()/count..(i+1)*selected.len()/count]).collect();
        let frequencies:Vec<f64>=groups.iter().map(|g|g.iter().map(|k|self.frequency_offsets_hz[*k]).sum::<f64>()/g.len() as f64+center_hz).collect();
        let avg=self.average_psd();
        let reduce=|p:&[f64]| -> Vec<f64> { groups.iter().map(|g| {
            let value=match self.config.detector.as_str() {
                "sample"=>p[g[g.len()/2]],
                "negative_peak"=>g.iter().map(|k|p[*k]).fold(f64::INFINITY,f64::min),
                "rms"=>g.iter().map(|k|p[*k]).sum::<f64>()/g.len() as f64,
                _=>g.iter().map(|k|p[*k]).fold(0.0,f64::max),
            };
            db(if value.is_finite(){value*self.enbw_hz}else{0.0})
        }).collect() };
        let levels=self.config.density_levels;
        let mut image_values=vec![0.0;count*levels];
        let display_denom=self.persistence_denominator();
        let rate=self.config.density_metric=="rate";
        let mut image_top:f64=0.0;
        for l in 0..levels {for (x,g) in groups.iter().enumerate() {
            let mut value=g.iter().map(|k|self.persistence_cell(*k,l)).sum::<f64>()/g.len() as f64;
            if rate{value/=display_denom.max(1.0);}
            image_values[(levels-1-l)*count+x]=value;image_top=image_top.max(value);
        }}
        let density_bytes:Vec<u8>=image_values.iter().map(|v|(255.0*(v/image_top.max(1e-30)).clamp(0.0,1.0).sqrt()).round() as u8).collect();
        let stats_denom=if self.config.stats_mode=="cumulative"{self.total_valid_frames as f64}else{display_denom};
        let limit_bytes=(self.config.max_density_history_mb*1024.0*1024.0) as usize;
        let capacity_frames=limit_bytes/self.config.fft_size;
        let requested_seconds=if self.config.persistence_unit=="frames"{
            self.config.persistence_frames as f64*self.hop as f64/self.config.sample_rate
        }else{self.config.persistence_seconds};
        let requested_frames=if self.config.persistence_unit=="frames"{self.config.persistence_frames}
            else{(requested_seconds*self.config.sample_rate/self.hop as f64-1e-12).ceil().max(0.0) as usize};
        let retained_seconds=match(self.window_entries.front(),self.window_entries.back()){
            (Some(first),Some(last))=>(last.time-first.time+self.hop as f64/self.config.sample_rate).max(0.0).min(requested_seconds),
            _=>0.0,
        };
        let mut stats_max:f64=0.0;
        for k in 0..self.config.fft_size {for l in 0..levels {
            let cell=k*levels+l;
            let mut value=if self.config.stats_mode=="cumulative"{self.cumulative[cell] as f64}else{self.persistence_cell(k,l)};
            if rate{value/=stats_denom.max(1.0);}
            stats_max=stats_max.max(value);
        }}
        let mut rows:Vec<Vec<f64>>=Vec::new();
        if self.waterfall_count>0 {rows.push(self.waterfall_sum.iter().map(|p|p/self.waterfall_count as f64).collect());}
        for row in self.waterfall.iter().rev() {if rows.len()>=self.config.waterfall_rows{break;}rows.push(row.iter().map(|p|*p as f64).collect());}
        if rows.is_empty(){rows.push(vec![0.0;self.config.fft_size]);}
        let mut wf_bytes=Vec::with_capacity(rows.len()*count);let mut gaps=Vec::new();
        for (y,row) in rows.iter().enumerate() {
            let gap=row.iter().any(|v|v.is_nan());if gap{gaps.push(y);}
            for g in &groups {
                let mean=if gap{0.0}else{g.iter().map(|k|row[*k]).sum::<f64>()/g.len() as f64};
                let scaled=(db(mean*self.enbw_hz)-(self.config.ref_level-self.config.range_db))/self.config.range_db;
                wf_bytes.push(if gap{0}else{(255.0*scaled.clamp(0.0,1.0)).round() as u8});
            }
        }
        json!({"spectrum":{"f_hz":frequencies,"live_db":reduce(&self.last_psd),"max_db":reduce(&self.max_psd),
            "min_db":reduce(&self.min_psd),"avg_db":reduce(&avg),"unit":"dBFS","bin_hz":self.bin_hz,
            "enbw_hz":self.enbw_hz,"detector":self.config.detector,"level_reference":"two-sided PSD times window ENBW"},
            "density":{"width":count,"height":levels,"data_b64":B64.encode(density_bytes),
                "min_db":self.config.density_min_db,"max_db":self.config.density_max_db,"metric":self.config.density_metric,
                "stats_mode":self.config.stats_mode,"normalization":"relative square-root color; all valid weighted analysis FFTs",
                "valid_frames":self.total_valid_frames,"effective_frames":stats_denom,"display_effective_frames":display_denom,
                "maximum_value":stats_max,"weighted_hits":self.config.stats_mode=="persistence" && self.config.persistence_mode=="exponential",
                "window_truncated":self.window_truncated,"window_frames_retained":self.window_entries.len(),
                "window_history_bytes":self.window_bytes,"window_seconds_retained":retained_seconds,
                "window_history_limit_bytes":limit_bytes,"window_capacity_frames":capacity_frames,
                "window_capacity_seconds":capacity_frames as f64*self.hop as f64/self.config.sample_rate,
                "window_requested_seconds":requested_seconds,"window_requested_frames":requested_frames,
                "window_memory_limit_expected":requested_frames>capacity_frames,
                "window_history_limit_scope":"level-index payload; excludes fixed histograms and container overhead",
                "orientation":"high_level_at_top"},
            "waterfall":{"width":count,"height":rows.len(),"data_b64":B64.encode(wf_bytes),
                "min_db":self.config.ref_level-self.config.range_db,"max_db":self.config.ref_level,
                "row_seconds":self.config.waterfall_row_seconds,"gap_rows":gaps,"orientation":"newest_at_top"},
            "valid_frames":self.total_valid_frames,"invalid_frames":self.invalid_frames,"gap_samples":self.gap_samples,
            "bin_hz":self.bin_hz,"enbw_hz":self.enbw_hz})
    }

    pub fn measurements(&self,low:f64,high:f64,center:f64,obw:f64,marker_count:usize)->Value {
        let unavailable=|reason:&str|json!({"status":"unavailable","reason":reason,"cp_dbfs":null,"cp_avg_dbfs":null,
            "obw_hz":null,"obw_low_hz":null,"obw_high_hz":null,"markers":[],"unit":"dBFS"});
        if self.last_frame_time.is_none(){return unavailable("No analysis frames");}
        if !low.is_finite() || !high.is_finite() || !center.is_finite() || low>=high
            || low<center-self.config.sample_rate/2.0 || high>center+self.config.sample_rate/2.0
            || !obw.is_finite() || obw<=0.0 || obw>=100.0 {return unavailable("Invalid measurement band or OBW percentage");}
        let n=self.config.fft_size;
        let frequency:Vec<f64>=self.frequency_offsets_hz.iter().map(|f|f+center).collect();
        let edges_low:Vec<f64>=frequency.iter().map(|f|(f-self.bin_hz/2.0).max(low)).collect();
        let widths:Vec<f64>=frequency.iter().zip(edges_low.iter()).map(|(f,l)|((f+self.bin_hz/2.0).min(high)-l).max(0.0)).collect();
        let selected:Vec<usize>=(0..n).filter(|k|widths[*k]>0.0).collect();
        let powers:Vec<f64>=selected.iter().map(|k|self.last_psd[*k]*widths[*k]).collect();
        let total:f64=powers.iter().sum();
        if total<=0.0{return unavailable("No finite power in measurement band");}
        let average:f64=self.avg_acc.iter().zip(widths.iter()).map(|(p,w)|p/self.avg_weight.max(1e-30)*w).sum();
        let quantile=|fraction:f64|{
            let target=total*fraction;let mut previous=0.0;
            for (j,p) in powers.iter().enumerate(){if previous+p>=target || j==powers.len()-1 {
                let k=selected[j];return edges_low[k]+((target-previous)/p.max(1e-300)).clamp(0.0,1.0)*widths[k];
            }previous+=p;}high
        };
        let tail=(1.0-obw/100.0)/2.0;let left=quantile(tail);let right=quantile(1.0-tail);
        let mut floor:Vec<f64>=selected.iter().map(|k|self.last_psd[*k]).collect();floor.sort_by(f64::total_cmp);
        let median=floor[floor.len()/2];let maxp=floor[floor.len()-1];let threshold=(median*4.0).max(maxp*1e-12).max(1e-29);
        let mut local:Vec<usize>=(0..n).filter(|k|widths[*k]>0.0 && self.last_psd[*k]>threshold
            && self.last_psd[*k]>=self.last_psd[(*k+n-1)%n] && self.last_psd[*k]>self.last_psd[(*k+1)%n]).collect();
        local.sort_by(|a,b|self.last_psd[*b].total_cmp(&self.last_psd[*a]));
        let mut chosen=Vec::<usize>::new();
        for k in local {if chosen.len()>=marker_count{break;}if chosen.iter().all(|j|k.abs_diff(*j)>=2){chosen.push(k);}}
        let mut markers=Vec::<Value>::new();let mut primary=(0.0,0.0);
        for (rank,k) in chosen.into_iter().enumerate(){
            let mut correction=0.0;
            if k>0 && k+1<n {let a=self.last_psd[k-1].max(1e-300).ln();let b=self.last_psd[k].max(1e-300).ln();let c=self.last_psd[k+1].max(1e-300).ln();let div=a-2.0*b+c;
                if div.abs()>1e-15{correction=(0.5*(a-c)/div).clamp(-0.5,0.5);}}
            let f=frequency[k]+correction*self.bin_hz;let level=db(self.last_psd[k]*self.enbw_hz);
            if rank==0{primary=(f,level);}
            markers.push(json!({"rank":rank+1,"frequency_hz":f,"level_dbfs":level,
                "delta_hz":f-primary.0,"delta_db":level-primary.1}));
        }
        json!({"status":"ok","cp_dbfs":db(total),"cp_avg_dbfs":db(average),"obw_hz":right-left,
            "obw_low_hz":left,"obw_high_hz":right,"obw_percent":obw,"markers":markers,"unit":"dBFS",
            "marker_min_separation_bins":2,"marker_threshold_db_above_median":10.0_f64*4.0_f64.log10()})
    }
}

fn lowpass(fs:f64,cutoff:f64,length:usize)->Vec<f32> {
    let length=length.max(17)|1;let middle=(length-1) as f64/2.0;
    let mut taps:Vec<f64>=(0..length).map(|i|{let t=2.0*cutoff/fs*(i as f64-middle);
        let sinc=if t.abs()<1e-15{1.0}else{(PI*t).sin()/(PI*t)};
        2.0*cutoff/fs*sinc*(0.54-0.46*(TAU*i as f64/(length-1) as f64).cos())}).collect();
    let sum:f64=taps.iter().sum();taps.iter_mut().for_each(|x|*x/=sum);taps.into_iter().map(|v|v as f32).collect()
}

struct Fir {
    taps:usize, fft_len:usize, history:Vec<Complex32>, response:Vec<Complex32>,
    forward:Arc<dyn Fft<f32>>, inverse:Arc<dyn Fft<f32>>, work:Vec<Complex32>,scratch:Vec<Complex32>,
}
impl Fir {
    fn new(taps:Vec<f32>)->Self {
        let fft_len=(4096usize.max(taps.len()*8)).next_power_of_two();
        let mut planner=FftPlanner::<f32>::new();let forward=planner.plan_fft_forward(fft_len);let inverse=planner.plan_fft_inverse(fft_len);
        let mut response=vec![Complex32::new(0.0,0.0);fft_len];for (i,t) in taps.iter().enumerate(){response[i].re=*t;}
        forward.process(&mut response);
        let scratch_len=forward.get_inplace_scratch_len().max(inverse.get_inplace_scratch_len());
        Self {taps:taps.len(),fft_len,history:vec![Complex32::new(0.0,0.0);taps.len()-1],response,
            forward,inverse,work:vec![Complex32::new(0.0,0.0);fft_len],scratch:vec![Complex32::new(0.0,0.0);scratch_len]}
    }
    fn process(&mut self,input:&[Complex32])->Vec<Complex32> {
        let history_len=self.taps-1;let stride=self.fft_len-history_len;let mut output=Vec::with_capacity(input.len());
        for part in input.chunks(stride){
            self.work.fill(Complex32::new(0.0,0.0));
            self.work[..history_len].copy_from_slice(&self.history);
            self.work[history_len..history_len+part.len()].copy_from_slice(part);
            self.history.copy_from_slice(&self.work[part.len()..part.len()+history_len]);
            self.forward.process_with_scratch(&mut self.work,&mut self.scratch);
            for i in 0..self.fft_len{self.work[i]*=self.response[i];}
            self.inverse.process_with_scratch(&mut self.work,&mut self.scratch);
            let scale=1.0/self.fft_len as f32;
            output.extend(self.work[history_len..history_len+part.len()].iter().map(|v|v*scale));
        }
        output
    }
}

struct OnePole {a:f64,state:f64}
impl OnePole {
    fn new(fs:f64,seconds:f64)->Self{Self{a:(-1.0/(fs*seconds)).exp(),state:0.0}}
    fn value(&mut self,x:f64)->f64{self.state=self.a*self.state+(1.0-self.a)*x;self.state}
}

pub struct VfoDemodulator {
    pub sample_rate:f64,pub frequency_offset_hz:f64,pub bandwidth_hz:f64,pub mode:String,pub audio_rate:u32,
    pub channel_rate:f64,pub factor:usize,
    channel_filter:Fir,audio_filter:Fir,dc:OnePole,deemphasis:Option<OnePole>,
    phase:f64,input_count:u64,previous_iq:Option<Complex32>,channel_count:u64,output_count:u64,previous_audio:Option<f64>,
}
impl VfoDemodulator {
    pub fn new(fs:f64,offset:f64,bw:f64,mode:&str,audio_rate:u32)->Self {
        Self::try_new(fs,offset,bw,mode,audio_rate).expect("VFO parameters must be validated")
    }
    pub fn try_new(fs:f64,offset:f64,bw:f64,mode:&str,audio_rate:u32)->Result<Self,String> {
        let mode=mode.to_uppercase();
        if !["AM","NFM","WFM"].contains(&mode.as_str()) || !fs.is_finite() || !offset.is_finite() || !bw.is_finite()
            || fs<=0.0 || audio_rate==0 || bw<=0.0 || bw>=fs || offset.abs()+bw/2.0>fs/2.0 {
            return Err("VFOの方式・帯域・周波数が不正です".into());
        }
        let target=(bw*1.25).max(audio_rate as f64*4.0);let factor=((fs/target) as usize).max(1);let channel_rate=fs/factor as f64;
        let transition=(bw*0.25).min(channel_rate/2.0-bw/2.0).max(500.0);
        let input_taps=((4.0*fs/transition) as usize).clamp(65,4095);
        let cutoff=(audio_rate as f64*0.45).min(if mode=="WFM"{15000.0}else{5000.0_f64.min(bw*0.4)});
        let audio_taps=((4.0*channel_rate/(audio_rate as f64/2.0-cutoff).max(1000.0)) as usize).clamp(65,1023);
        Ok(Self {sample_rate:fs,frequency_offset_hz:offset,bandwidth_hz:bw,mode:mode.clone(),audio_rate,channel_rate,factor,
            channel_filter:Fir::new(lowpass(fs,bw/2.0,input_taps)),audio_filter:Fir::new(lowpass(channel_rate,cutoff,audio_taps)),
            dc:OnePole::new(channel_rate,0.05),deemphasis:if mode=="WFM"{Some(OnePole::new(audio_rate as f64,50e-6))}else{None},
            phase:0.0,input_count:0,previous_iq:None,channel_count:0,output_count:0,previous_audio:None})
    }
    pub fn reset(&mut self){*self=Self::new(self.sample_rate,self.frequency_offset_hz,self.bandwidth_hz,&self.mode,self.audio_rate);}
    pub fn process(&mut self,iq:&[Complex32])->Vec<f32> {
        if iq.is_empty(){return Vec::new();}
        let omega=-TAU*self.frequency_offset_hz/self.sample_rate;
        let (mut im,mut re)=self.phase.sin_cos();let (rotate_im,rotate_re)=omega.sin_cos();
        let mut mixed=Vec::with_capacity(iq.len());
        for (index,x) in iq.iter().enumerate(){
            mixed.push(Complex32::new((x.re as f64*re-x.im as f64*im) as f32,(x.re as f64*im+x.im as f64*re) as f32));
            let next_re=re*rotate_re-im*rotate_im;im=im*rotate_re+re*rotate_im;re=next_re;
            if index%2048==2047{let norm=(re*re+im*im).sqrt();re/=norm;im/=norm;}
        }
        self.phase=(self.phase+omega*iq.len() as f64).rem_euclid(TAU);
        let filtered=self.channel_filter.process(&mixed);
        let offset=((self.factor as u64-self.input_count%self.factor as u64)%self.factor as u64) as usize;
        let channel:Vec<Complex32>=filtered.iter().skip(offset).step_by(self.factor).copied().collect();
        self.input_count+=iq.len() as u64;
        if channel.is_empty(){return Vec::new();}
        let mut audio=Vec::with_capacity(channel.len());
        if self.mode=="AM" {
            for z in &channel{let envelope=z.norm() as f64;let dc=self.dc.value(envelope);audio.push(Complex32::new(((envelope-dc)/dc.max(1e-4)) as f32,0.0));}
        }else{
            let deviation=if self.mode=="WFM"{75000.0}else{5000.0_f64.min(self.bandwidth_hz*0.2)};
            for z in &channel{let previous=self.previous_iq.unwrap_or(*z);let diff=(*z*previous.conj()).arg() as f64;
                self.previous_iq=Some(*z);let value=diff*self.channel_rate/(TAU*deviation);let dc=self.dc.value(value);audio.push(Complex32::new((value-dc) as f32,0.0));}
        }
        let filtered=self.audio_filter.process(&audio);
        let start=self.channel_count as f64;let end=start+filtered.len() as f64-1.0;let step=self.channel_rate/self.audio_rate as f64;
        let next=self.output_count as f64*step;
        let number=if next<=end+1e-9{(((end-next)/step+1e-9).floor() as usize)+1}else{0};
        let mut result=Vec::with_capacity(number);
        let output_gain=demod_audio_scale(&self.mode);
        for j in 0..number {
            let t=(self.output_count+j as u64) as f64*step-start;
            let left=t.floor() as i64;let fraction=t-left as f64;
            let a=if left<0{self.previous_audio.unwrap_or(filtered[0].re as f64)}else{filtered[(left as usize).min(filtered.len()-1)].re as f64};
            let right=(left+1).max(0) as usize;let b=filtered[right.min(filtered.len()-1)].re as f64;
            let mut value=a+(b-a)*fraction;
            if let Some(filter)=&mut self.deemphasis{value=filter.value(value);}
            // Do not hide saturation before the PCM writer can count it.
            result.push(if value.is_finite(){(value*output_gain) as f32}else{0.0});
        }
        self.previous_audio=filtered.last().map(|v|v.re as f64);self.channel_count+=filtered.len() as u64;self.output_count+=number as u64;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tone(n:usize,fs:f64,f:f64,amplitude:f32)->Vec<Complex32>{(0..n).map(|i|Complex32::from_polar(amplitude,(TAU*f*i as f64/fs) as f32)).collect()}
    #[test]
    fn fft_integral_and_all_frame_denominator(){
        let cfg=Config{sample_rate:64000.0,fft_size:1024,stats_mode:"cumulative".into(),..Config::default()};
        let mut a=Analyzer::new(cfg);let x=tone(17001,64000.0,4000.0,0.25);let r=a.process(&x,0);
        assert_eq!(r.frames_processed,1+(x.len()-1024)/512);
        assert!((a.last_psd.iter().sum::<f64>()*a.bin_hz-0.0625).abs()<1e-6);
        let (hist,denom)=a.density_statistics();for col in hist.chunks(a.config.density_levels){assert!((col.iter().sum::<f64>()-denom).abs()<1e-9);}
        let m=a.measurements(-20000.0,20000.0,0.0,99.0,6);
        assert!((m["cp_dbfs"].as_f64().unwrap()-20.0*0.25_f64.log10()).abs()<0.001);
        assert!((m["markers"][0]["frequency_hz"].as_f64().unwrap()-4000.0).abs()<0.1);
        let snap=a.snapshot(50000.0,0.0,80);
        assert_eq!(B64.decode(snap["density"]["data_b64"].as_str().unwrap()).unwrap().len(),80*128);
    }
    #[test]
    fn chunking_and_gap(){
        let cfg=Config{sample_rate:64000.0,fft_size:1024,..Config::default()};let x=tone(17001,64000.0,4000.0,0.25);
        let mut a=Analyzer::new(cfg.clone());let mut b=Analyzer::new(cfg);let whole=a.process(&x,0);let mut ends=Vec::new();let mut cursor=0;
        for length in [17,301,99,2048,7,6000,8529]{let end=(cursor+length).min(x.len());let r=b.process(&x[cursor..end],cursor as u64);ends.extend(r.fft_frame_end_samples);cursor=end;}
        assert_eq!(ends,whole.fft_frame_end_samples);assert_eq!(a.total_valid_frames,b.total_valid_frames);
        b.process(&x[..2048],20000);assert_eq!(b.gap_samples,2999);
        assert!(!b.snapshot(64000.0,0.0,32)["waterfall"]["gap_rows"].as_array().unwrap().is_empty());
    }
    #[test]
    fn persistence_modes_and_switching(){
        let cfg=Config{sample_rate:64000.0,fft_size:1024,persistence_mode:"window".into(),persistence_unit:"frames".into(),persistence_frames:3,..Config::default()};
        let mut a=Analyzer::new(cfg);a.process(&tone(20000,64000.0,4000.0,0.25),0);assert_eq!(a.density_statistics().1,3.0);
        let mut cfg=a.config.clone();cfg.persistence_mode="off".into();a.update_config(cfg).unwrap();a.process(&tone(2048,64000.0,4000.0,0.25),20000);assert_eq!(a.density_statistics().1,1.0);
        let mut cfg=a.config.clone();cfg.persistence_unit="time_constant".into();cfg.switch_policy="retain".into();assert_eq!(a.update_config(cfg).unwrap()["history_cleared"],true);assert_eq!(a.total_valid_frames,0);
        assert_eq!(fft_size_for_rbw(2048000.0,750.0,"hann"),4096);
    }
    #[test]
    fn trace_reset_restarts_from_live_without_erasing_other_analysis_history(){
        let cfg=Config{sample_rate:64000.0,fft_size:1024,..Config::default()};
        let mut a=Analyzer::new(cfg);
        a.process(&tone(4096,64000.0,4000.0,0.8),0);
        a.process(&tone(4096,64000.0,4000.0,0.2),4096);
        a.note_gap(37);
        let mut partial=tone(3000,64000.0,4000.0,0.2);partial[0]=Complex32::new(f32::NAN,0.0);
        a.process(&partial,8229);
        let bin=a.config.fft_size/2+64;
        let old_max=a.max_psd[bin];let live=a.last_psd.clone();
        assert!(old_max>live[bin]*10.0);
        assert!(!a.tail.is_empty());assert!(a.invalid_frames>0&&a.gap_samples>0);
        let before=a.snapshot(64000.0,0.0,128);
        let tail=a.tail.clone();let tail_start=a.tail_start;let expected=a.expected_next;
        let counters=(a.total_valid_frames,a.invalid_frames,a.gap_samples,a.last_frame_time);
        let config=serde_json::to_value(&a.config).unwrap();
        a.reset_traces();
        assert_eq!(a.max_psd,live);assert_eq!(a.min_psd,live);assert_eq!(a.average_psd(),live);
        assert_eq!(a.last_psd,live);assert_eq!(a.tail,tail);assert_eq!(a.tail_start,tail_start);assert_eq!(a.expected_next,expected);
        assert_eq!((a.total_valid_frames,a.invalid_frames,a.gap_samples,a.last_frame_time),counters);
        assert_eq!(serde_json::to_value(&a.config).unwrap(),config);
        let after=a.snapshot(64000.0,0.0,128);
        for key in ["density","waterfall","valid_frames","invalid_frames","gap_samples"]{assert_eq!(after[key],before[key],"{key}");}
        let measured=a.measurements(-16000.0,16000.0,0.0,99.0,6);
        assert_eq!(measured["cp_avg_dbfs"],measured["cp_dbfs"]);
        a.reset_traces();assert_eq!(a.snapshot(64000.0,0.0,128),after);
        // Preserve phase at the input block boundary so this verifies the
        // lower signal level rather than cancellation from a phase step.
        let rotation=Complex32::from_polar(1.0,(TAU*4000.0*3000.0/64000.0).rem_euclid(TAU) as f32);
        let continued_input:Vec<Complex32>=tone(2048,64000.0,4000.0,0.1).into_iter().map(|v|v*rotation).collect();
        let continued=a.process(&continued_input,11229);
        assert!(continued.frames_processed>0);
        assert!(a.max_psd[bin]<=live[bin]*(1.0+1e-6));assert!(a.max_psd[bin]<old_max/4.0);
        assert!((a.min_psd[bin]*a.enbw_hz-0.01).abs()<1e-6);
        assert!(a.average_psd()[bin]>0.0&&a.average_psd()[bin]<live[bin]);
        assert_eq!(a.gap_samples,counters.2);assert_eq!(a.invalid_frames,counters.1);
    }
    #[test]
    fn trace_reset_before_first_fft_keeps_partial_input_and_has_no_fake_level(){
        let mut a=Analyzer::new(Config{sample_rate:64000.0,fft_size:1024,..Config::default()});
        let input=tone(1024,64000.0,4000.0,0.25);
        a.process(&input[..512],0);a.reset_traces();a.reset_traces();
        let empty=a.snapshot(64000.0,0.0,64);
        for name in ["live_db","max_db","min_db","avg_db"]{
            assert!(empty["spectrum"][name].as_array().unwrap().iter().all(|v|v.as_f64().is_some_and(|level|level.is_finite()&&level<=-290.0)));
        }
        assert_eq!(a.process(&input[512..],512).frames_processed,1);
        assert_eq!(a.max_psd,a.last_psd);assert_eq!(a.min_psd,a.last_psd);assert_eq!(a.average_psd(),a.last_psd);
        let bin=a.config.fft_size/2+64;assert!((a.last_psd[bin]*a.enbw_hz-0.0625).abs()<1e-6);
    }
    #[test]
    fn rbw_selector_respects_valid_fft_bounds_and_invalid_targets(){
        let fs=2_048_000.0;
        // Hann ENBW at the endpoints is 24000 Hz / 93.75 Hz. Requests
        // outside that achievable interval must saturate to a valid bound.
        assert_eq!(fft_size_for_rbw(fs,24_000.0,"hann"),128);
        assert_eq!(fft_size_for_rbw(fs,1e9,"hann"),128);
        assert_eq!(fft_size_for_rbw(fs,93.75,"hann"),32768);
        assert_eq!(fft_size_for_rbw(fs,1.0,"hann"),32768);
        assert_eq!(fft_size_for_rbw(fs,16_000.0,"rect"),128);
        assert_eq!(fft_size_for_rbw(fs,750.0,"hann"),4096);
        for invalid in [0.0,-1.0,f64::NAN,f64::INFINITY,f64::NEG_INFINITY] {
            assert_eq!(fft_size_for_rbw(fs,invalid,"hann"),4096);
            assert_eq!(fft_size_for_rbw(invalid,750.0,"hann"),4096);
        }
        let smallest=Config{fft_size:128,..Config::default()};assert!(validate_config(&smallest).is_ok());
    }
    #[test]
    fn window_limit_is_visible_and_current(){
        let cfg=Config{sample_rate:64000.0,fft_size:1024,persistence_mode:"window".into(),
            persistence_seconds:100.0,max_density_history_mb:0.01,..Config::default()};
        let x=tone(64000,64000.0,4000.0,0.25);
        let mut a=Analyzer::new(cfg.clone());a.process(&x,0);
        let snap=a.snapshot(64000.0,0.0,32);let density=&snap["density"];
        assert_eq!(density["window_capacity_frames"],10);assert_eq!(density["window_frames_retained"],10);
        assert!((density["window_seconds_retained"].as_f64().unwrap()-0.08).abs()<1e-9);
        assert_eq!(density["window_history_limit_bytes"],10485);assert_eq!(density["window_requested_seconds"],100.0);
        assert_eq!(density["window_truncated"],true);assert_eq!(density["window_memory_limit_expected"],true);
        // A display-only change cannot erase an active memory-limit warning.
        let mut changed=a.config.clone();changed.ref_level=-30.0;a.update_config(changed).unwrap();
        assert_eq!(a.snapshot(64000.0,0.0,32)["density"]["window_truncated"],true);
        let mut changed=a.config.clone();changed.persistence_seconds=0.016;a.update_config(changed).unwrap();
        let snap=a.snapshot(64000.0,0.0,32);let density=&snap["density"];
        assert_eq!(density["window_frames_retained"],2);
        assert!((density["window_seconds_retained"].as_f64().unwrap()-0.016).abs()<1e-9);
        assert_eq!(density["window_truncated"],false);assert_eq!(density["window_memory_limit_expected"],false);
        let mut normal=Analyzer::new(Config{persistence_seconds:0.08,..cfg.clone()});normal.process(&x,0);
        assert_eq!(normal.snapshot(64000.0,0.0,32)["density"]["window_truncated"],false);
        let mut frames=Analyzer::new(Config{persistence_unit:"frames".into(),persistence_frames:20,..cfg});frames.process(&x,0);
        let mut changed=frames.config.clone();changed.ref_level=-30.0;frames.update_config(changed).unwrap();
        assert_eq!(frames.snapshot(64000.0,0.0,32)["density"]["window_truncated"],true);
        let mut changed=frames.config.clone();changed.persistence_frames=5;frames.update_config(changed).unwrap();
        assert_eq!(frames.snapshot(64000.0,0.0,32)["density"]["window_truncated"],false);
    }
    #[test]
    fn all_demodulation_modes_recover_1khz(){
        let fs=512000.0;let n=(fs*0.25) as usize;
        for (mode,bw,deviation) in [("AM",12000.0,0.0),("NFM",25000.0,5000.0),("WFM",200000.0,75000.0)]{
            let x:Vec<Complex32>=(0..n).map(|i|{let t=i as f64/fs;let angle=TAU*74000.0*t+deviation/1000.0*(TAU*1000.0*t).sin();let amp=if mode=="AM"{0.15*(1.0+0.5*(TAU*1000.0*t).cos())}else{0.15};Complex32::from_polar(amp as f32,angle as f32)}).collect();
            let mut d=VfoDemodulator::new(fs,74000.0,bw,mode,32000);let mut audio=Vec::new();for chunk in x.chunks(17003){audio.extend(d.process(chunk));}
            assert!((audio.len() as i64-8000).abs()<=1);let audio=&audio[3200..];
            let mut re=0.0;let mut im=0.0;let mut energy=0.0;
            for (i,v) in audio.iter().enumerate(){let phase=TAU*1000.0*i as f64/32000.0;re+=*v as f64*phase.cos();im+=*v as f64*phase.sin();energy+=(*v as f64).powi(2);}
            let fraction=2.0*(re*re+im*im)/(audio.len() as f64*energy);assert!(fraction>0.9,"{mode}: tone fraction {fraction}");
        }
    }
    #[test]
    fn fixed_wfm_headroom_and_no_hidden_limiter(){
        assert_eq!(demod_audio_scale("WFM"),0.5);assert_eq!(demod_audio_scale("AM"),1.0);assert_eq!(demod_audio_scale("NFM"),1.0);
        let fs=512000.0;let n=(fs*0.25) as usize;
        let generate=|deviation:f64|->Vec<Complex32>{(0..n).map(|i|{let t=i as f64/fs;let phase=TAU*74000.0*t+deviation/1000.0*(TAU*1000.0*t).sin();Complex32::from_polar(0.15,phase as f32)}).collect()};
        let mut wfm=VfoDemodulator::new(fs,74000.0,200000.0,"WFM",32000);let audio=wfm.process(&generate(75000.0));let steady=&audio[3200..];
        let rms=(steady.iter().map(|v|(*v as f64).powi(2)).sum::<f64>()/steady.len() as f64).sqrt();
        assert!((0.32..0.36).contains(&rms),"WFM fixed headroom rms={rms}");
        assert!(steady.iter().all(|v|v.abs()<0.55));
        let mut nfm=VfoDemodulator::new(fs,74000.0,50000.0,"NFM",32000);let outlier=nfm.process(&generate(10000.0));
        assert!(outlier[3200..].iter().any(|v|v.abs()>1.2),"Out-of-range audio must reach PCM sink unclamped");
    }
}
