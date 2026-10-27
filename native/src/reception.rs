//! Optional listening-path processing. It never modifies IQ or analysis PSD.
//! RF squelch, fades and limiter advance by samples, not API block boundaries.
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};

#[derive(Clone,Debug,Serialize,Deserialize,PartialEq)]
#[serde(default,deny_unknown_fields)]
pub struct ReceptionConfig {
    pub afc_enabled:bool,
    pub afc_max_hz:f64,
    pub squelch_enabled:bool,
    pub squelch_dbfs:f64,
    pub squelch_hysteresis_db:f64,
    pub squelch_hang_ms:u32,
    pub audio_gain_db:f64,
    pub limiter_enabled:bool,
    pub limiter_ceiling_dbfs:f64,
    pub click_suppression_enabled:bool,
    pub wav_audio:String,
}
impl Default for ReceptionConfig {
    fn default()->Self {Self {afc_enabled:false,afc_max_hz:1000.0,squelch_enabled:false,squelch_dbfs:-60.0,
        squelch_hysteresis_db:3.0,squelch_hang_ms:300,audio_gain_db:0.0,limiter_enabled:true,
        limiter_ceiling_dbfs:-2.0,click_suppression_enabled:true,wav_audio:"demod".into()}}
}
impl ReceptionConfig {pub fn validate(&self)->Result<(),String>{validate_config(self)}}
pub fn validate_config(c:&ReceptionConfig)->Result<(),String>{
    for(value,low,high,label)in [(c.afc_max_hz,1.0,5000.0,"AFC上限Hz"),(c.squelch_dbfs,-160.0,10.0,"RFスケルチdBFS"),
        (c.squelch_hysteresis_db,0.0,30.0,"スケルチhysteresis dB"),(c.audio_gain_db,-24.0,36.0,"音声gain dB"),
        (c.limiter_ceiling_dbfs,-24.0,0.0,"Limiter上限dBFS")]{
        if !value.is_finite()||value<low||value>high{return Err(format!("{label}は{low}〜{high}の有限値が必要です"));}
    }
    if c.squelch_hang_ms>5000{return Err("スケルチhangは0〜5000 msです".into());}
    if !["demod","processed"].contains(&c.wav_audio.as_str()){return Err("WAV音声はdemod/processedです".into());}
    Ok(())
}

pub struct AudioProcessor {
    pub config:ReceptionConfig,
    sample_rate:u32,
    squelch_open:bool,
    hang_remaining:u64,
    gate_gain:f64,
    limiter_gain:f64,
    rf_dbfs:Option<f64>,
    last_conditioned:Option<f64>,
    boundary_pending:bool,
    repair_offset:f64,
    repair_remaining:u64,
    pre_overload_samples:u64,
    limited_samples:u64,
    muted_samples:u64,
    invalid_samples:u64,
    boundary_repairs:u64,
    gap_count:u64,
    processed_samples:u64,
}
impl AudioProcessor {
    pub fn new(config:ReceptionConfig,sample_rate:u32)->Self{Self::try_new(config,sample_rate).expect("AudioProcessor config must be validated")}
    pub fn try_new(config:ReceptionConfig,sample_rate:u32)->Result<Self,String>{
        config.validate()?;if sample_rate==0||sample_rate>384000{return Err("音声sample rateが不正です".into());}
        let open=!config.squelch_enabled;let gain=if open&&!config.click_suppression_enabled{1.0}else{0.0};
        Ok(Self {config,sample_rate,squelch_open:open,hang_remaining:0,gate_gain:gain,limiter_gain:1.0,rf_dbfs:None,
            last_conditioned:None,boundary_pending:false,repair_offset:0.0,repair_remaining:0,pre_overload_samples:0,
            limited_samples:0,muted_samples:0,invalid_samples:0,boundary_repairs:0,gap_count:0,processed_samples:0})
    }
    pub fn set_config(&mut self,config:ReceptionConfig)->Result<(),String>{
        config.validate()?;if config==self.config{return Ok(());}
        if !config.limiter_enabled{self.limiter_gain=1.0;}
        if !config.click_suppression_enabled{self.repair_remaining=0;self.boundary_pending=false;}
        self.config=config;Ok(())
    }
    pub fn update_config(&mut self,config:ReceptionConfig)->Result<(),String>{self.set_config(config)}
    /// Known source discontinuity: fades the next listening samples from zero.
    /// Counters are retained, including one new gap event.
    pub fn note_gap(&mut self){
        self.gap_count=self.gap_count.saturating_add(1);self.squelch_open=!self.config.squelch_enabled;self.hang_remaining=0;
        self.gate_gain=if self.squelch_open&&!self.config.click_suppression_enabled{1.0}else{0.0};
        self.last_conditioned=None;self.boundary_pending=false;self.repair_remaining=0;self.limiter_gain=1.0;self.rf_dbfs=None;
    }
    /// Only an explicitly known waveform/frame discontinuity can request a
    /// three-millisecond step repair. Arbitrary process() chunks cannot.
    pub fn note_boundary(&mut self){self.boundary_pending=true;}
    /// Reset accounting only; does not reopen squelch or alter audible state.
    pub fn reset_stats(&mut self){
        self.pre_overload_samples=0;self.limited_samples=0;self.muted_samples=0;self.invalid_samples=0;
        self.boundary_repairs=0;self.gap_count=0;self.processed_samples=0;
    }
    pub fn process(&mut self,audio:&[f32],rf_dbfs:f64)->Vec<f32>{
        if audio.is_empty(){return Vec::new();}
        self.rf_dbfs=rf_dbfs.is_finite().then_some(rf_dbfs);
        let gain=10.0_f64.powf(self.config.audio_gain_db/20.0);
        let ceiling=10.0_f64.powf(self.config.limiter_ceiling_dbfs/20.0);
        let fade_samples=(self.sample_rate as f64*0.004).round().max(1.0) as u64;
        let repair_samples=(self.sample_rate as f64*0.003).round().max(1.0) as u64;
        let hang_samples=self.sample_rate as u64*self.config.squelch_hang_ms as u64/1000;
        let release=(-1.0/(self.sample_rate as f64*0.230)).exp();
        let mut result=Vec::with_capacity(audio.len());
        for sample in audio {
            if !self.config.squelch_enabled{self.squelch_open=true;}
            else if !self.squelch_open {
                if self.rf_dbfs.is_some_and(|v|v>=self.config.squelch_dbfs){self.squelch_open=true;self.hang_remaining=hang_samples;}
            }else if self.rf_dbfs.is_some_and(|v|v>=self.config.squelch_dbfs-self.config.squelch_hysteresis_db){self.hang_remaining=hang_samples;}
            else if self.hang_remaining>0{self.hang_remaining-=1;}
            else{self.squelch_open=false;}
            let target=if self.squelch_open{1.0}else{0.0};
            if self.gate_gain<target{self.gate_gain=(self.gate_gain+1.0/fade_samples as f64).min(target);}
            else if self.gate_gain>target{self.gate_gain=(self.gate_gain-1.0/fade_samples as f64).max(target);}
            let raw=if sample.is_finite(){*sample as f64}else{self.invalid_samples=self.invalid_samples.saturating_add(1);0.0};
            let mut value=raw*gain;
            // Honest accounting before squelch/repair/limiting: input gain
            // would exceed the nominal PCM amplitude interval [-1,+1].
            if value.abs()>1.0{self.pre_overload_samples=self.pre_overload_samples.saturating_add(1);}
            if self.boundary_pending {
                self.boundary_pending=false;
                if self.config.click_suppression_enabled {
                    if let Some(previous)=self.last_conditioned {
                        if (value-previous).abs()>0.48 {
                            self.repair_offset=previous-value;self.repair_remaining=repair_samples;
                            self.boundary_repairs=self.boundary_repairs.saturating_add(1);
                        }
                    }
                }
            }
            if self.repair_remaining>0 {
                value+=self.repair_offset*self.repair_remaining as f64/repair_samples as f64;
                self.repair_remaining-=1;
            }
            self.last_conditioned=Some(value);value*=self.gate_gain;
            if self.gate_gain<=1e-12{self.muted_samples=self.muted_samples.saturating_add(1);}
            if self.config.limiter_enabled {
                let desired=if value.abs()>ceiling{ceiling/value.abs()}else{1.0};
                if desired<self.limiter_gain{self.limiter_gain=desired;}
                else{self.limiter_gain=desired+(self.limiter_gain-desired)*release;}
                if self.limiter_gain<1.0-1e-9{self.limited_samples=self.limited_samples.saturating_add(1);}
                value*=self.limiter_gain;
            }
            // This only guards floating representation overflow with the
            // limiter OFF; it is not an audio/PCM amplitude hard limiter.
            if value.abs()>f32::MAX as f64{self.invalid_samples=self.invalid_samples.saturating_add(1);value=value.clamp(-(f32::MAX as f64),f32::MAX as f64);}
            result.push(value as f32);self.processed_samples=self.processed_samples.saturating_add(1);
        }
        result
    }
    pub fn status(&self)->Value{
        let db=|value:f64|20.0*value.max(1e-6).log10();
        let limiter_db=20.0*self.limiter_gain.max(1e-300).log10();
        json!({"rf_dbfs":self.rf_dbfs,"rf_available":self.rf_dbfs.is_some(),"squelch_open":self.squelch_open,
            "squelch_gain":self.gate_gain,"squelch_gain_db":db(self.gate_gain),"audio_gain_db":self.config.audio_gain_db,
            "limiter_gain_db":limiter_db,"limiter_reduction_db":-limiter_db,
            "pre_overload_samples":self.pre_overload_samples,"limited_samples":self.limited_samples,"muted_samples":self.muted_samples,
            "invalid_samples":self.invalid_samples,"boundary_repairs":self.boundary_repairs,"gap_count":self.gap_count,
            "processed_samples":self.processed_samples,"fade_active":(self.squelch_open&&self.gate_gain<1.0)||(!self.squelch_open&&self.gate_gain>0.0),
            "sample_rate":self.sample_rate,"fade_ms":4,"boundary_repair_ms":3,"limiter_release_seconds":0.230,
            "pre_overload_definition":"abs(demod audio times configured gain)>1 before mute/repair/limiter",
            "boundary_repair_policy":"explicit note_boundary only; process chunks are not discontinuities",
            "engineering_defaults":true})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unprocessed()->ReceptionConfig{ReceptionConfig{limiter_enabled:false,click_suppression_enabled:false,..ReceptionConfig::default()}}
    #[test]
    fn quiet_unity_and_no_hidden_clip_when_limiter_disabled(){
        let mut p=AudioProcessor::new(unprocessed(),32000);let input=[0.1,-0.2,0.0,1.5,-2.0];
        assert_eq!(p.process(&input,-40.0),input);assert_eq!(p.status()["pre_overload_samples"],2);assert_eq!(p.status()["limited_samples"],0);
        let mut p=AudioProcessor::new(ReceptionConfig{click_suppression_enabled:false,..ReceptionConfig::default()},32000);
        assert_eq!(p.process(&[0.25;500],-40.0),vec![0.25;500]);assert_eq!(p.status()["limiter_gain_db"],0.0);
    }
    #[test]
    fn limiter_attack_release_and_accounting_are_sample_continuous(){
        let cfg=ReceptionConfig{click_suppression_enabled:false,..ReceptionConfig::default()};let mut a=AudioProcessor::new(cfg.clone(),32000);let mut b=AudioProcessor::new(cfg,32000);
        let mut input=vec![0.1;37];input.extend(vec![2.0;13]);input.extend(vec![0.1;16000]);
        let whole=a.process(&input,-30.0);let mut split=Vec::new();for chunk in input.chunks(71){split.extend(b.process(chunk,-30.0));}
        assert_eq!(whole,split);assert_eq!(a.status(),b.status());let ceiling=10.0_f64.powf(-2.0/20.0);
        assert!(whole.iter().all(|v|v.abs()as f64<=ceiling+1e-6));assert_eq!(a.status()["pre_overload_samples"],13);
        assert!(whole[50]<0.05);assert!(whole.last().unwrap()>&0.09);assert!(a.status()["limited_samples"].as_u64().unwrap()>13);
    }
    #[test]
    fn squelch_hysteresis_hang_fades_and_gap_follow_sample_clock(){
        let cfg=ReceptionConfig{squelch_enabled:true,squelch_hang_ms:10,limiter_enabled:false,..ReceptionConfig::default()};let mut p=AudioProcessor::new(cfg,32000);
        assert!(p.process(&[0.2;100],-80.0).iter().all(|v|*v==0.0));assert_eq!(p.status()["squelch_open"],false);
        let open=p.process(&[0.2;160],-59.0);assert!(open[0]>0.0&&open[0]<0.01);assert_eq!(open[127],0.2);
        p.process(&[0.2;500],-62.0);assert_eq!(p.status()["squelch_open"],true);
        p.process(&[0.2;320],-70.0);assert_eq!(p.status()["squelch_open"],true);
        p.process(&[0.2;129],-70.0);assert_eq!(p.status()["squelch_open"],false);assert_eq!(p.status()["squelch_gain"],0.0);
        assert!(p.process(&[0.2;10],-62.0).iter().all(|v|*v==0.0));let reopen=p.process(&[0.2;128],-59.0);assert!(reopen[0]<0.01);
        p.note_gap();assert_eq!(p.status()["gap_count"],1);assert_eq!(p.status()["rf_available"],false);assert!(p.process(&[0.2;1],-59.0)[0]<0.01);
    }
    #[test]
    fn high_frequency_tone_and_arbitrary_chunks_do_not_request_step_repairs(){
        let cfg=ReceptionConfig{limiter_enabled:false,..ReceptionConfig::default()};let mut a=AudioProcessor::new(cfg.clone(),32000);let mut b=AudioProcessor::new(cfg,32000);
        let input:Vec<f32>=(0..10000).map(|i|(0.8*(std::f64::consts::TAU*11000.0*i as f64/32000.0).sin())as f32).collect();
        let whole=a.process(&input,-30.0);let mut split=Vec::new();for chunk in input.chunks(53){split.extend(b.process(chunk,-30.0));}
        assert_eq!(whole,split);assert_eq!(a.status()["boundary_repairs"],0);assert_eq!(a.status(),b.status());assert_eq!(&whole[128..],&input[128..]);
        a.note_boundary();let repaired=a.process(&[-0.8;100],-30.0);assert_eq!(a.status()["boundary_repairs"],1);assert!((repaired[0]-whole.last().unwrap()).abs()<1e-6);assert_eq!(repaired[96],-0.8);
    }
    #[test]
    fn invalid_config_noop_statistics_reset_and_nonfinite_audio_are_safe(){
        let mut p=AudioProcessor::new(ReceptionConfig::default(),32000);p.process(&[0.2;23],-40.0);let before=p.status();p.set_config(p.config.clone()).unwrap();assert_eq!(p.status(),before);
        for value in [f64::NAN,f64::INFINITY,-30.0,40.0]{let mut cfg=p.config.clone();cfg.audio_gain_db=value;assert!(p.set_config(cfg).is_err());assert_eq!(p.status(),before);}
        let mut cfg=p.config.clone();cfg.wav_audio="unknown".into();assert!(p.set_config(cfg).is_err());
        let output=p.process(&[f32::NAN,f32::INFINITY,0.2],f64::NAN);assert!(output.iter().all(|v|v.is_finite()));assert_eq!(p.status()["invalid_samples"],2);assert_eq!(p.status()["rf_available"],false);
        let gain=p.status()["squelch_gain"].clone();p.reset_stats();assert_eq!(p.status()["processed_samples"],0);assert_eq!(p.status()["squelch_gain"],gain);
    }
    #[test]
    fn squelch_hang_and_reopening_are_independent_of_input_chunk_sizes(){
        let cfg=ReceptionConfig{squelch_enabled:true,squelch_hang_ms:10,..ReceptionConfig::default()};let mut a=AudioProcessor::new(cfg.clone(),32000);let mut b=AudioProcessor::new(cfg,32000);
        let mut whole=Vec::new();let mut split=Vec::new();
        for(length,rf)in [(1000,-80.0),(333,-59.0),(300,-62.0),(1280,-80.0),(500,-59.0)]{
            let input=vec![0.2;length];whole.extend(a.process(&input,rf));for chunk in input.chunks(49){split.extend(b.process(chunk,rf));}
        }
        assert_eq!(whole,split);assert_eq!(a.status(),b.status());assert_eq!(a.status()["squelch_open"],true);assert_eq!(a.status()["boundary_repairs"],0);
    }
    #[test]
    fn extreme_float_inputs_are_limited_finitely_with_honest_pre_overload_counts(){
        let mut p=AudioProcessor::new(ReceptionConfig{audio_gain_db:36.0,click_suppression_enabled:false,..ReceptionConfig::default()},32000);
        let input=[f32::MAX,-f32::MAX,f32::NAN,f32::INFINITY];let bits=input.map(f32::to_bits);let output=p.process(&input,-30.0);
        assert_eq!(input.map(f32::to_bits),bits);let ceiling=10.0_f32.powf(-2.0/20.0);
        assert!(output.iter().all(|v|v.is_finite()&&v.abs()<=ceiling+1e-6));assert_eq!(p.status()["pre_overload_samples"],2);assert_eq!(p.status()["invalid_samples"],2);
        assert!(p.status()["limiter_reduction_db"].as_f64().unwrap()>0.0);
    }
}
