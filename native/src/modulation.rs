//! Optional, conservative analog-modulation evidence from neutral complex IQ.
//!
//! Feed raw decoded IQ before mode/AFC/sideband filtering. `bandwidth_hz` is an
//! independent observation bandwidth. Evidence is a rule match, not probability.
//! Mathematical basis: https://www.mathworks.com/help/comm/ug/analog-passband-modulation.html
//! Carrier-relative SSB: https://en.lntwww.lnt.ei.tum.de/Modulation_Methods/Single-Sideband_Modulation
//! Neither source establishes this heuristic's accuracy. Unknown/mixed inputs
//! can be observationally indistinguishable; manual adoption is always required.
use rustfft::{num_complex::Complex32,Fft,FftPlanner};
use serde::{Deserialize,Serialize};
use std::{f64::consts::TAU,sync::Arc,time::{Duration,Instant}};

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ModulationConfig {
    pub enabled:bool,
    pub sample_rate:f64,
    pub frequency_offset_hz:f64,
    pub bandwidth_hz:f64,
    pub carrier_reference_explicit:bool,
    pub band_complete:bool,
    pub dc_affected:bool,
}
impl Default for ModulationConfig {fn default()->Self{Self{enabled:false,sample_rate:2_048_000.,frequency_offset_hz:0.,bandwidth_hz:200_000.,carrier_reference_explicit:false,band_complete:true,dc_affected:false}}}

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="lowercase")]
pub enum EstimateState {Disabled,Collecting,Candidate,Hold}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="UPPERCASE")]
pub enum ModulationCandidate {Am,Fm,Ssb,Usb,Lsb}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="lowercase")]
pub enum EvidenceStrength {None,Weak,Moderate,Strong}

#[derive(Clone,Debug,Serialize)]
pub struct ModulationFeatures {
    pub quality_db:f64,
    pub envelope_cv:f64,
    pub phase_spread_hz:f64,
    pub phase_valid_fraction:f64,
    pub carrier_fraction:f64,
    pub sideband_balance_db:f64,
    pub sideband_direction:Option<String>,
    pub occupied_bandwidth_hz:f64,
    pub fft_bin_hz:f64,
}
#[derive(Clone,Debug,Serialize)]
pub struct ModulationEstimate {
    pub state:EstimateState,
    pub candidate:Option<ModulationCandidate>,
    pub proposed_mode:Option<String>,
    pub evidence:EvidenceStrength,
    pub reasons:Vec<String>,
    pub features:Option<ModulationFeatures>,
    pub observed_seconds:f64,
    pub updated_sample:Option<u64>,
}
impl ModulationEstimate {
    fn hold(state:EstimateState,reason:&str)->Self{Self{state,candidate:None,proposed_mode:None,evidence:EvidenceStrength::None,reasons:vec![reason.into()],features:None,observed_seconds:0.,updated_sample:None}}
}

pub struct ModulationEstimator {
    config:ModulationConfig,
    result:ModulationEstimate,
    buffer:Vec<Complex32>,
    forward:Option<Arc<dyn Fft<f32>>>,
    inverse:Option<Arc<dyn Fft<f32>>>,
    scratch:Vec<Complex32>,
    window_size:usize,
    window_start:u64,
    next_capture:u64,
    expected_sample:Option<u64>,
    last_data:Option<Instant>,
    previous_candidate:Option<ModulationCandidate>,
    stable_windows:usize,
}
impl ModulationEstimator {
    pub fn new(config:ModulationConfig)->Result<Self,String>{
        Self::validate(&config)?;
        let mut value=Self{config,result:ModulationEstimate::hold(EstimateState::Disabled,"disabled"),buffer:vec![],forward:None,inverse:None,scratch:vec![],window_size:0,window_start:0,next_capture:0,expected_sample:None,last_data:None,previous_candidate:None,stable_windows:0};
        value.reset("awaiting_samples");Ok(value)
    }
    fn validate(c:&ModulationConfig)->Result<(),String>{
        if !c.sample_rate.is_finite()||c.sample_rate<=0.||!c.frequency_offset_hz.is_finite()||!c.bandwidth_hz.is_finite()||!(6000.0..=250000.0).contains(&c.bandwidth_hz){return Err("Invalid modulation observation rate/offset/bandwidth".into());}Ok(())
    }
    pub fn configure(&mut self,config:ModulationConfig)->Result<bool,String>{
        Self::validate(&config)?;if config==self.config{return Ok(false)}
        self.config=config;self.forward=None;self.inverse=None;self.scratch.clear();self.window_size=0;self.reset("configuration_changed");
        if !self.config.enabled{self.buffer.shrink_to_fit();self.scratch.shrink_to_fit();}Ok(true)
    }
    pub fn reset(&mut self,reason:&str){
        self.buffer.clear();self.expected_sample=None;self.next_capture=0;self.last_data=None;self.previous_candidate=None;self.stable_windows=0;
        self.result=ModulationEstimate::hold(if self.config.enabled{EstimateState::Collecting}else{EstimateState::Disabled},if self.config.enabled{reason}else{"disabled"});
    }
    pub fn estimate(&self)->&ModulationEstimate{&self.result}
    pub fn snapshot(&self)->ModulationEstimate{
        if self.config.enabled&&self.last_data.is_some_and(|at|at.elapsed()>Duration::from_millis(1500)){
            let mut result=ModulationEstimate::hold(EstimateState::Hold,"stale_samples");result.updated_sample=self.result.updated_sample;return result;
        }self.result.clone()
    }
    pub fn observe(&mut self,start_sample:u64,iq:&[Complex32])->bool{
        if !self.config.enabled{return false;}
        let end=start_sample.saturating_add(iq.len()as u64);
        if iq.is_empty(){return false;}
        if self.last_data.is_some_and(|at|at.elapsed()>Duration::from_millis(1500)){
            self.buffer.clear();self.next_capture=start_sample;self.previous_candidate=None;self.stable_windows=0;
            self.result=ModulationEstimate::hold(EstimateState::Collecting,"sampling_resumed");
        }
        self.last_data=Some(Instant::now());
        if self.expected_sample.is_some_and(|expected|expected!=start_sample){
            self.buffer.clear();self.expected_sample=Some(end);self.next_capture=end;
            self.publish_hold("sample_gap",end);return true;
        }
        self.expected_sample=Some(end);
        let c=&self.config;
        let blocked=if !c.band_complete||c.frequency_offset_hz.abs()+c.bandwidth_hz/2.>c.sample_rate/2.{Some("band_incomplete")}
            else if c.frequency_offset_hz.abs()+c.bandwidth_hz>c.sample_rate/2.{Some("noise_reference_out_of_band")}
            else if c.dc_affected{Some("center_dc_affected")}
            else if c.sample_rate/131072.>c.bandwidth_hz/16.{Some("sampling_resolution_insufficient")}else{None};
        if let Some(reason)=blocked{self.buffer.clear();self.publish_hold(reason,end);return true;}
        if self.window_size==0{
            self.window_size=((c.sample_rate*0.064).clamp(256.,131072.).ceil()as usize).next_power_of_two().min(131072);
            let mut planner=FftPlanner::<f32>::new();let f=planner.plan_fft_forward(self.window_size);let r=planner.plan_fft_inverse(self.window_size);
            self.scratch.resize(f.get_inplace_scratch_len().max(r.get_inplace_scratch_len()),Complex32::new(0.,0.));
            self.forward=Some(f);self.inverse=Some(r);self.buffer.reserve(self.window_size);
        }
        let mut cursor=0;let mut changed=false;
        while cursor<iq.len(){
            let at=start_sample.saturating_add(cursor as u64);
            if self.buffer.is_empty(){
                if at<self.next_capture{cursor+=((self.next_capture-at).min((iq.len()-cursor)as u64))as usize;continue;}
                self.window_start=at;
            }
            let count=(self.window_size-self.buffer.len()).min(iq.len()-cursor);
            self.buffer.extend_from_slice(&iq[cursor..cursor+count]);cursor+=count;
            if self.buffer.len()==self.window_size{
                let window_end=self.window_start.saturating_add(self.window_size as u64);
                self.analyze(window_end);changed=true;
                self.next_capture=self.window_start.saturating_add((self.config.sample_rate*0.5).round().max(self.window_size as f64)as u64);
            }
        }changed
    }
    fn publish_hold(&mut self,reason:&str,end:u64){
        self.previous_candidate=None;self.stable_windows=0;
        self.result=ModulationEstimate::hold(EstimateState::Hold,reason);self.result.updated_sample=Some(end);
    }
    fn analyze(&mut self,end:u64){
        let mut values=std::mem::take(&mut self.buffer);
        let c=self.config.clone();let n=values.len();let bin=c.sample_rate/n as f64;
        let invalid=values.iter().any(|z|!z.re.is_finite()||!z.im.is_finite());
        let rails=values.iter().filter(|z|z.re.abs()>=0.995||z.im.abs()>=0.995).count();
        if invalid||rails as f64/n as f64>0.001{
            self.publish_hold(if invalid{"non_finite_samples"}else{"clipping_suspected"},end);values.clear();self.buffer=values;return;
        }
        let omega=-TAU*c.frequency_offset_hz/c.sample_rate;let (step_im,step_re)=omega.sin_cos();let(mut re,mut im)=(1.,0.);
        for(index,value)in values.iter_mut().enumerate(){
            *value=Complex32::new((value.re as f64*re-value.im as f64*im)as f32,(value.re as f64*im+value.im as f64*re)as f32);
            let next=re*step_re-im*step_im;im=im*step_re+re*step_im;re=next;
            if index%2048==2047{let length=re.hypot(im);re/=length;im/=length;}
        }
        self.forward.as_ref().unwrap().process_with_scratch(&mut values,&mut self.scratch);
        let scale=1./(n as f64*n as f64);
        let power:Vec<f64>=values.iter().map(|z|z.norm_sqr()as f64*scale).collect();
        let frequency=|index:usize|if index<=n/2{index as f64*bin}else{(index as f64-n as f64)*bin};
        let band:Vec<usize>=(0..n).filter(|i|frequency(*i).abs()<=c.bandwidth_hz/2.).collect();
        let mut guard:Vec<f64>=(0..n).filter(|i|{let f=frequency(*i).abs();f>=c.bandwidth_hz*0.6&&f<=c.bandwidth_hz*0.95}).map(|i|power[i]).collect();
        if guard.len()<16||band.len()<16{self.publish_hold("sampling_resolution_insufficient",end);values.clear();self.buffer=values;return;}
        let peak=*band.iter().max_by(|a,b|power[**a].total_cmp(&power[**b])).unwrap();
        let noise=quantile(&mut guard,0.5).max(power[peak]*1e-10).max(1e-24);
        let total=band.iter().map(|i|power[*i]).sum::<f64>();let expected_noise=noise*band.len()as f64;
        let excess=(total-expected_noise).max(0.);let quality=10.*(excess/expected_noise).max(1e-24).log10();
        if !quality.is_finite()||quality<8.||excess<1e-18{self.publish_hold("signal_quality_insufficient",end);values.clear();self.buffer=values;return;}
        let edge=band.iter().filter(|i|frequency(**i).abs()>c.bandwidth_hz*0.45).map(|i|(power[*i]-noise).max(0.)).sum::<f64>();
        if edge/excess>0.12{self.publish_hold("signal_at_band_edge",end);values.clear();self.buffer=values;return;}
        let mut sorted:Vec<(f64,f64)>=band.iter().map(|i|(frequency(*i),(power[*i]-noise).max(0.))).collect();sorted.sort_by(|a,b|a.0.total_cmp(&b.0));
        let occupied=power_quantile(&sorted,0.995)-power_quantile(&sorted,0.005);
        let multiple_clusters=separated_signals(&sorted,excess,c.bandwidth_hz);
        let carrier_frequency=frequency(peak);let carrier_radius=(2.5*bin).max(30.);
        let carrier=band.iter().filter(|i|(frequency(**i)-carrier_frequency).abs()<=carrier_radius).map(|i|(power[*i]-noise).max(0.)).sum::<f64>();
        let carrier_fraction=(carrier/excess).clamp(0.,1.);
        let guard_hz=(3.*bin).max(80.);
        let sides=|reference:f64|{
            let(mut upper,mut lower)=(0.,0.);
            for i in &band{let relative=frequency(*i)-reference;let p=(power[*i]-noise).max(0.);if relative>guard_hz{upper+=p;}else if relative< -guard_hz{lower+=p;}}
            (upper,lower)
        };
        let (upper,lower)=sides(0.);let balance=(10.*((upper+excess*1e-10)/(lower+excess*1e-10)).log10()).clamp(-100.,100.);
        let (am_upper,am_lower)=sides(carrier_frequency);
        let am_balance=10.*((am_upper+excess*1e-10)/(am_lower+excess*1e-10)).log10();
        // A symmetric, mode-independent spectral extraction. The outer quarter
        // of each finite burst is discarded to avoid circular FFT boundary data.
        for (index,z)in values.iter_mut().enumerate(){if frequency(index).abs()>c.bandwidth_hz/2.{*z=Complex32::new(0.,0.);}}
        self.inverse.as_ref().unwrap().process_with_scratch(&mut values,&mut self.scratch);
        let factor=(c.sample_rate/(2.5*c.bandwidth_hz)).floor().max(1.)as usize;let rate=c.sample_rate/factor as f64;
        let mut amplitude=Vec::new();let mut phase_rate=Vec::new();let mut previous=None;let mut pairs=0;
        for z in values[n/4..3*n/4].iter().step_by(factor){
            let z=*z/(n as f32);let a=z.norm()as f64;amplitude.push(a);
            if let Some(last)=previous{pairs+=1;let last:Complex32=last;if (z.norm_sqr()as f64)>expected_noise*2.&&(last.norm_sqr()as f64)>expected_noise*2.{let d=z*last.conj();phase_rate.push(d.im.atan2(d.re)as f64*rate/TAU);}}
            previous=Some(z);
        }
        let mean=amplitude.iter().sum::<f64>()/amplitude.len().max(1)as f64;
        let variance=amplitude.iter().map(|x|(x-mean).powi(2)).sum::<f64>()/amplitude.len().max(1)as f64;
        let envelope_cv=variance.sqrt()/mean.max(1e-30);
        let valid_fraction=phase_rate.len()as f64/pairs.max(1)as f64;
        let mut sorted_rate=phase_rate.clone();let phase_spread=if sorted_rate.len()>=64{(quantile(&mut sorted_rate,0.95)-quantile(&mut sorted_rate,0.05)).abs()}else{0.};
        let phase_continuous=continuous_frequency(&phase_rate);
        let features=ModulationFeatures{quality_db:quality.min(60.),envelope_cv,phase_spread_hz:phase_spread,phase_valid_fraction:valid_fraction,carrier_fraction,
            sideband_balance_db:balance,sideband_direction:if balance>10.{Some("upper".into())}else if balance< -10.{Some("lower".into())}else{None},occupied_bandwidth_hz:occupied,fft_bin_hz:bin};
        let cw=carrier_fraction>0.97||(carrier_fraction>0.85&&envelope_cv<0.06&&phase_spread<(4.*bin).max(80.));
        // Natural FM sidebands can form separated strong spectral clusters,
        // especially near a carrier Bessel null. A spectral gap alone cannot
        // establish independent stations: evaluate neutral-IQ envelope/phase
        // first, retaining the cluster hold for inputs without FM consistency.
        let fm_consistent=envelope_cv<0.08&&valid_fraction>0.9&&phase_spread>(8.*bin).max(180.)&&phase_continuous;
        let candidate=if cw||multiple_clusters&&!fm_consistent{None}
            else if envelope_cv>0.06&&envelope_cv<0.85&&carrier_fraction>0.15&&am_balance.abs()<5.&&valid_fraction>0.75&&phase_spread<(4.*bin).max(80.){Some(ModulationCandidate::Am)}
            else if fm_consistent{Some(ModulationCandidate::Fm)}
            else if balance.abs()>10.&&envelope_cv>0.08&&carrier_fraction<0.97{
                Some(if c.carrier_reference_explicit{if balance>0.{ModulationCandidate::Usb}else{ModulationCandidate::Lsb}}else{ModulationCandidate::Ssb})
            }else{None};
        let mut result=ModulationEstimate::hold(EstimateState::Hold,if cw{"unmodulated_or_cw"}else if multiple_clusters&&!fm_consistent{"multiple_signals_suspected"}else{"ambiguous_features"});
        result.features=Some(features);result.observed_seconds=n as f64/c.sample_rate;result.updated_sample=Some(end);
        if let Some(kind)=candidate{
            if self.previous_candidate==Some(kind){self.stable_windows+=1;}else{self.stable_windows=1;}
            self.previous_candidate=Some(kind);
            if self.stable_windows<2{result.state=EstimateState::Collecting;result.reasons=vec!["stability_pending".into()];}
            else{
                result.candidate=Some(kind);result.evidence=if self.stable_windows>=3&&quality>=15.{EvidenceStrength::Strong}else{EvidenceStrength::Moderate};
                result.state=if kind==ModulationCandidate::Ssb{EstimateState::Hold}else{EstimateState::Candidate};
                result.reasons=vec![if kind==ModulationCandidate::Ssb{"carrier_reference_unconfirmed"}else if kind==ModulationCandidate::Fm{"fm_family_only"}else if matches!(kind,ModulationCandidate::Usb|ModulationCandidate::Lsb){"sideband_relative_to_vfo_reference"}else{"am_features_consistent"}.into()];
                result.proposed_mode=match kind{ModulationCandidate::Am=>Some("AM".into()),ModulationCandidate::Usb=>Some("USB".into()),ModulationCandidate::Lsb=>Some("LSB".into()),_=>None};
            }
        }else{self.previous_candidate=None;self.stable_windows=0;}
        self.result=result;values.clear();self.buffer=values;
    }
}

fn quantile(values:&mut[f64],q:f64)->f64{values.sort_by(f64::total_cmp);values[((values.len()-1)as f64*q).round()as usize]}
fn power_quantile(values:&[(f64,f64)],q:f64)->f64{let total=values.iter().map(|v|v.1).sum::<f64>();let mut seen=0.;for(f,p)in values{seen+=p;if seen>=q*total{return *f;}}values.last().map(|v|v.0).unwrap_or(0.)}
fn separated_signals(values:&[(f64,f64)],total:f64,bw:f64)->bool{
    let mut peaks:Vec<(f64,f64)>=values.iter().copied().filter(|v|v.1>total*0.002).collect();peaks.sort_by(|a,b|b.1.total_cmp(&a.1));peaks.truncate(24);
    for (i,(a,_))in peaks.iter().enumerate(){let pa=values.iter().filter(|(f,_)|(f-a).abs()<bw*0.08).map(|v|v.1).sum::<f64>();if pa<total*0.22{continue;}
        for(b,_)in &peaks[i+1..]{if (a-b).abs()<bw*0.3{continue;}let pb=values.iter().filter(|(f,_)|(f-b).abs()<bw*0.08).map(|v|v.1).sum::<f64>();let middle=(a+b)/2.;let gap=values.iter().filter(|(f,_)|(f-middle).abs()<bw*0.05).map(|v|v.1).sum::<f64>();if pb>total*0.22&&gap<total*0.02{return true;}}
    }false
}
fn continuous_frequency(values:&[f64])->bool{
    if values.len()<128{return false;}let mut sorted=values.to_vec();let low=quantile(&mut sorted,0.01);let high=quantile(&mut sorted,0.99);if high-low<1.{return false;}
    let mut bins=[0usize;16];for value in values{let k=(((value-low)/(high-low)*16.).floor().max(0.)as usize).min(15);bins[k]+=1;}
    bins.iter().filter(|count|**count>values.len()/1000).count()>=8&&(*bins.iter().max().unwrap()as f64/values.len()as f64)<0.4
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(fs:f64,bw:f64)->ModulationConfig{ModulationConfig{enabled:true,sample_rate:fs,bandwidth_hz:bw,..Default::default()}}
    fn drive(c:ModulationConfig,seconds:f64,mut sample:impl FnMut(f64)->Complex32)->ModulationEstimator{
        let fs=c.sample_rate;let mut e=ModulationEstimator::new(c).unwrap();let total=(seconds*fs)as usize;let mut at=0;
        while at<total{let count=(total-at).min(4096);let values:Vec<_>=(at..at+count).map(|i|sample(i as f64/fs)).collect();e.observe(at as u64,&values);at+=count;}e
    }
    fn am(t:f64,offset:f64)->Complex32{let a=0.25*(1.+0.5*(TAU*1000.*t).cos()+0.15*(TAU*2300.*t).sin());Complex32::from_polar(a as f32,(TAU*offset*t).rem_euclid(TAU)as f32)}
    fn fm(t:f64,offset:f64,deviation:f64)->Complex32{let phase=TAU*offset*t+deviation/1000.*(TAU*1000.*t).sin();Complex32::from_polar(0.25,phase.rem_euclid(TAU)as f32)}
    fn ssb(t:f64,side:f64,cfo:f64)->Complex32{
        [(350.,0.25),(980.,0.18),(2450.,0.22)].iter().fold(Complex32::new(0.,0.),|sum,(f,a)|sum+Complex32::from_polar(*a as f32,(TAU*(side*f+cfo)*t).rem_euclid(TAU)as f32))
    }
    fn noise(seed:&mut u64)->Complex32{let mut next=||{*seed=seed.wrapping_mul(6364136223846793005).wrapping_add(1);((*seed>>32)as u32 as f64/u32::MAX as f64-0.5)as f32};Complex32::new(next(),next())}
    fn assert_hold(e:&ModulationEstimator){assert_eq!(e.estimate().state,EstimateState::Hold,"{:?}",e.estimate());assert!(e.estimate().proposed_mode.is_none());}

    #[test]
    fn off_is_no_work_and_does_not_create_fft_or_capture_buffers(){
        let mut e=ModulationEstimator::new(Default::default()).unwrap();assert!(!e.observe(0,&[Complex32::new(0.2,0.1);64]));assert_eq!(e.estimate().state,EstimateState::Disabled);assert!(e.forward.is_none()&&e.buffer.capacity()==0);
    }
    #[test]
    fn invalid_config_is_rejected_without_replacing_existing_config(){
        let mut e=ModulationEstimator::new(config(128000.,20000.)).unwrap();let original=e.config.clone();
        for (fs,bw,offset)in [(f64::NAN,20000.,0.),(128000.,f64::INFINITY,0.),(128000.,5999.,0.),(128000.,250001.,0.),(128000.,20000.,f64::NAN)]{
            let mut bad=original.clone();bad.sample_rate=fs;bad.bandwidth_hz=bw;bad.frequency_offset_hz=offset;assert!(e.configure(bad).is_err());assert_eq!(e.config,original);
        }
    }
    #[test]
    fn clear_am_with_carrier_and_cfo_has_provisional_am_evidence(){
        let e=drive(config(128000.,20000.),1.2,|t|am(t,700.));assert_eq!(e.estimate().candidate,Some(ModulationCandidate::Am),"{:?}",e.estimate());assert_eq!(e.estimate().proposed_mode.as_deref(),Some("AM"));
    }
    #[test]
    fn narrow_and_wide_fm_are_family_only_without_claiming_submode(){
        for(fs,bw,deviation)in [(128000.,20000.,2500.),(512000.,200000.,75000.)]{
            let e=drive(config(fs,bw),1.2,|t|fm(t,500.,deviation));assert_eq!(e.estimate().candidate,Some(ModulationCandidate::Fm),"dev{deviation}: {:?}",e.estimate());assert!(e.estimate().proposed_mode.is_none());
        }
    }
    #[test]
    fn explicit_carrier_reference_allows_only_provisional_usb_lsb(){
        for(side,expected)in [(1.,ModulationCandidate::Usb),(-1.,ModulationCandidate::Lsb)]{
            let mut c=config(64000.,12000.);c.carrier_reference_explicit=true;let e=drive(c,1.2,|t|ssb(t,side,0.));assert_eq!(e.estimate().candidate,Some(expected),"{:?}",e.estimate());assert!(e.estimate().reasons.contains(&"sideband_relative_to_vfo_reference".into()));
        }
    }
    #[test]
    fn no_reference_keeps_sided_evidence_but_holds_mode_adoption(){
        let e=drive(config(64000.,12000.),1.2,|t|ssb(t,1.,0.));assert_hold(&e);assert_eq!(e.estimate().candidate,Some(ModulationCandidate::Ssb));assert_eq!(e.estimate().features.as_ref().unwrap().sideband_direction.as_deref(),Some("upper"));assert!(e.estimate().reasons.contains(&"carrier_reference_unconfirmed".into()));
    }
    #[test]
    fn pure_noise_and_a_signal_below_noise_are_held(){
        let mut seed=5;let e=drive(config(128000.,20000.),1.2,|_|noise(&mut seed)*0.3);assert_hold(&e);assert!(e.estimate().candidate.is_none());
        let mut seed=11;let e=drive(config(128000.,20000.),1.2,|t|am(t,0.)*0.01+noise(&mut seed)*0.3);assert_hold(&e);assert!(e.estimate().candidate.is_none());
    }
    #[test]
    fn cw_and_a_single_tone_ssb_counterexample_cannot_claim_modulation(){
        let mut c=config(64000.,12000.);c.carrier_reference_explicit=true;
        for hz in [0.,1000.,1234.5]{let e=drive(c.clone(),1.2,|t|Complex32::from_polar(0.25,(TAU*hz*t).rem_euclid(TAU)as f32));assert_hold(&e);assert!(e.estimate().candidate.is_none(),"{hz}: {:?}",e.estimate());}
    }
    #[test]
    fn separated_mixed_channels_are_held_instead_of_forced_to_a_class(){
        let e=drive(config(512000.,200000.),1.2,|t|am(t,-60000.)*0.6+am(t,60000.)*0.6);assert_hold(&e);assert!(e.estimate().reasons.contains(&"multiple_signals_suspected".into()),"{:?}",e.estimate());
    }
    #[test]
    fn edge_incomplete_dc_and_insufficient_sample_width_are_holds(){
        for which in 0..4{let mut c=config(64000.,12000.);match which{0=>c.band_complete=false,1=>c.dc_affected=true,2=>c.sample_rate=8000.,_=>c.frequency_offset_hz=25000.}
            let e=drive(c,0.1,|t|am(t,0.));assert_hold(&e);assert!(e.estimate().candidate.is_none());assert!(e.forward.is_none());}
        let e=drive(config(64000.,12000.),1.2,|t|am(t,5800.));assert_hold(&e);assert!(e.estimate().reasons.contains(&"signal_at_band_edge".into()));
    }
    #[test]
    fn a_complete_single_window_is_not_yet_a_stable_recommendation(){
        let e=drive(config(128000.,20000.),0.1,|t|am(t,0.));assert_eq!(e.estimate().state,EstimateState::Collecting);assert!(e.estimate().candidate.is_none());
    }
    #[test]
    fn gap_stale_and_changed_reference_discard_old_candidates(){
        let mut e=drive(config(128000.,20000.),1.2,|t|am(t,0.));assert_eq!(e.estimate().candidate,Some(ModulationCandidate::Am));
        let at=e.expected_sample.unwrap()+1000;e.observe(at,&[Complex32::new(0.2,0.);512]);assert_hold(&e);assert!(e.estimate().reasons.contains(&"sample_gap".into()));assert!(e.buffer.is_empty());
        let mut e=drive(config(128000.,20000.),1.2,|t|am(t,0.));e.last_data=Some(Instant::now()-Duration::from_secs(2));assert_eq!(e.snapshot().state,EstimateState::Hold);assert!(e.snapshot().reasons.contains(&"stale_samples".into()));
        let mut changed=e.config.clone();changed.carrier_reference_explicit=true;assert!(e.configure(changed).unwrap());assert!(e.estimate().candidate.is_none());assert_eq!(e.estimate().state,EstimateState::Collecting);
    }
    #[test]
    fn offsets_use_nominal_reference_not_selected_demod_or_afc(){
        let mut c=config(128000.,20000.);c.frequency_offset_hz=24000.;let shifted=drive(c,1.2,|t|am(t,24700.));let base=drive(config(128000.,20000.),1.2,|t|am(t,700.));assert_eq!(shifted.estimate().candidate,base.estimate().candidate);assert_eq!(shifted.estimate().candidate,Some(ModulationCandidate::Am));
    }
    #[test]
    fn corrupted_and_clipped_iq_are_not_modulation_evidence(){
        for x in [Complex32::new(f32::NAN,0.),Complex32::new(1.,1.)]{let e=drive(config(64000.,12000.),0.1,|_|x);assert_hold(&e);assert!(e.estimate().candidate.is_none());}
    }
    #[test]
    fn changing_carrier_ref_can_make_side_labels_ambiguous(){
        let mut c=config(64000.,12000.);c.carrier_reference_explicit=true;let e=drive(c,1.2,|t|ssb(t,1.,-2000.));assert_hold(&e);assert!(e.estimate().candidate.is_none());
    }
    #[test]
    fn results_are_finite_serializable_evidence_not_probability(){
        let e=drive(config(128000.,20000.),1.2,|t|am(t,0.));let value=serde_json::to_value(e.snapshot()).unwrap();assert!(value.get("probability").is_none());assert!(value["evidence"].is_string());assert!(value["features"]["quality_db"].as_f64().unwrap().is_finite());
    }
}
