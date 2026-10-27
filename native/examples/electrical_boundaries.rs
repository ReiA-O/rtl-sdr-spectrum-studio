//! Offline probe of the product's already-built Rust library. No DLL/server/RF.
use rtl_spectrum_studio::{device, dsp::{Analyzer,Config,VfoDemodulator,fft_size_for_rbw},frontend::CenterDcRejector,recording::WavRecorder};
use rustfft::num_complex::Complex32;
use serde_json::{json,Value};
use std::{f64::consts::TAU,fs::{self,OpenOptions},io::Write,path::PathBuf};

fn tone(fs:f64,hz:f64,n:usize,amplitude:f64,dc:Complex32)->Vec<Complex32>{
    (0..n).map(|i|{let p=TAU*hz*i as f64/fs;dc+Complex32::new((amplitude*p.cos())as f32,(amplitude*p.sin())as f32)}).collect()
}
fn gain(x:&[Complex32],fs:f64,hz:f64,start:usize,amplitude:f64)->f64{
    let(mut re,mut im)=(0.0,0.0);for(i,z)in x.iter().enumerate(){let p=TAU*hz*(start+i)as f64/fs;re+=z.re as f64*p.cos()+z.im as f64*p.sin();im+=z.im as f64*p.cos()-z.re as f64*p.sin();}
    re.hypot(im)/x.len()as f64/amplitude
}
fn db(value:f64)->f64{10.0*value.max(1e-30).log10()}
fn check(checks:&mut Vec<Value>,name:&str,ok:bool,evidence:Value){checks.push(json!({"name":name,"ok":ok,"evidence":evidence}));}
fn audio_fraction(audio:&[f32],hz:f64)->f64{
    let(mut re,mut im,mut power)=(0.0,0.0,0.0);for(i,x)in audio.iter().enumerate(){let p=TAU*hz*i as f64/32000.0;re+=*x as f64*p.cos();im+=*x as f64*p.sin();power+=(*x as f64).powi(2);}
    2.0*(re*re+im*im)/(audio.len()as f64*power.max(1e-30))
}
fn analog(fs:f64,offset:f64,n:usize,mode:&str)->Vec<Complex32>{
    (0..n).map(|i|{let t=i as f64/fs;let m=(TAU*1000.0*t).sin();let phase=TAU*offset*t+match mode{"WFM"=>75.0*m,"NFM"=>2.5*m,_=>0.0};
        let a=if mode=="AM"{0.15*(1.0+0.5*m)}else{0.15};Complex32::new((a*phase.cos())as f32,(a*phase.sin())as f32)}).collect()
}
fn demod(fs:f64,offset:f64,bw:f64,mode:&str,input:&[Complex32])->Vec<f32>{
    let mut d=VfoDemodulator::try_new(fs,offset,bw,mode,32000).unwrap();let mut audio=Vec::new();for part in input.chunks(17003){audio.extend(d.process(part));}audio
}
fn main(){
    let out=PathBuf::from(std::env::args_os().nth(1).expect("output JSON path required"));let folder=out.parent().unwrap();fs::create_dir_all(folder).unwrap();
    let mut checks=Vec::new();
    for fs in [1_024_000.0,1_536_000.0,2_048_000.0,2_400_000.0]{
        let mut dc=vec![Complex32::new(0.2,-0.1);131072];CenterDcRejector::new(fs).process_in_place(&mut dc);
        check(&mut checks,"DC removed from first block",dc.iter().all(|z|z.re==0.0&&z.im==0.0),json!({"fs":fs}));
        for hz in [-1000.0,-20.0,1.0,5.0,20.0,50.0,100.0,1000.0,fs*0.45]{
            let n=(fs*0.3)as usize;let skip=(fs*0.15)as usize;let mut x=tone(fs,hz,n,0.1,Complex32::new(0.2,-0.1));
            CenterDcRejector::new(fs).process_in_place(&mut x);let measured=gain(&x[skip..],fs,hz,skip,0.1);
            let a=(-TAU*20.0/fs).exp();let w=TAU*hz/fs;let expected=(1.0+a)*0.5*((2.0-2.0*w.cos())/(1.0+a*a-2.0*a*w.cos())).sqrt();
            check(&mut checks,"Frontend gain matches discrete high-pass transfer",(measured-expected).abs()<0.0001&&x.iter().all(|z|z.re.is_finite()&&z.im.is_finite()),json!({"fs":fs,"offset_hz":hz,"measured_gain":measured,"expected_gain":expected,"attenuation_db":20.0*measured.max(1e-30).log10()}));
        }
        let mut reject=CenterDcRejector::new(fs);let mut seed=vec![Complex32::new(0.0,0.0);4096];reject.process_in_place(&mut seed);
        let mut step=vec![Complex32::new(0.9,-0.9);(fs*0.2)as usize];reject.process_in_place(&mut step);
        let first=(step[0].re as f64).hypot(step[0].im as f64);let tail=step.last().unwrap();
        check(&mut checks,"DC step stays finite and settles",first<1.28&&tail.norm()<0.000001&&step.iter().all(|z|z.re.is_finite()&&z.im.is_finite()),json!({"fs":fs,"initial_complex_magnitude":first,"final_complex_magnitude":tail.norm()}));
    }
    for fs in [1_024_000.0,2_400_000.0]{for n in [128usize,4096,32768]{for window in ["rect","hann","blackman"]{
        let cfg=Config{sample_rate:fs,fft_size:n,window:window.into(),overlap:0.0,density_levels:16,waterfall_rows:1,max_density_history_mb:1.0,..Config::default()};
        let mut analyzer=Analyzer::new(cfg);let mut start=0;
        for k in [-(n as i64)/2,-(n as i64)/2+1,-((n as f64*0.45).floor()as i64),0,(n as f64*0.45).floor()as i64,n as i64/2-1,n as i64/2]{
            let hz=k as f64*fs/n as f64;let input=tone(fs,hz,n,0.2,Complex32::new(0.0,0.0));let result=analyzer.process(&input,start);start+=n as u64;
            let expected=db(0.04);let expected_k=if k==n as i64/2{-(n as i64)/2}else{k};let peak_index=analyzer.last_psd.iter().enumerate().max_by(|a,b|a.1.total_cmp(b.1)).unwrap().0;
            let actual_hz=analyzer.frequency_offsets_hz[peak_index];
            check(&mut checks,"FFT edge tone has correct power and half-open frequency bin",(result.integrated_dbfs-expected).abs()<0.00002&&(result.peak_dbfs-expected).abs()<0.00002&&(actual_hz-expected_k as f64*fs/n as f64).abs()<0.00001,
                json!({"fs":fs,"fft_size":n,"window":window,"input_offset_hz":hz,"expected_wrapped_offset_hz":expected_k as f64*fs/n as f64,"actual_peak_offset_hz":actual_hz,"integrated_dbfs":result.integrated_dbfs,"peak_per_enbw_dbfs":result.peak_dbfs}));
        }
        let target_below=1e-6;let target_above=fs*10.0;
        check(&mut checks,"RBW requests saturate at supported FFT bounds",fft_size_for_rbw(fs,target_below,window)==32768&&fft_size_for_rbw(fs,target_above,window)==128,json!({"fs":fs,"window":window,"low_target_hz":target_below,"high_target_hz":target_above}));
    }}}
    // Verify the reusable lower-level API over the FULL Nyquist span as well as
    // the product's provisional +/-0.45Fs usable band.
    let fs=2048000.0;let n=4096;let mut analyzer=Analyzer::new(Config{overlap:0.0,window:"rect".into(),density_levels:16,..Config::default()});
    let input=tone(fs,-fs/2.0,n,0.2,Complex32::new(0.0,0.0));let result=analyzer.process(&input,0);let measurement=analyzer.measurements(-fs/2.0,fs/2.0,0.0,99.0,1);
    check(&mut checks,"Full Nyquist ROI retains complete periodic bin power",(measurement["cp_dbfs"].as_f64().unwrap()-result.integrated_dbfs).abs()<0.00001,
        json!({"total_dbfs":result.integrated_dbfs,"measured_full_roi_cp_dbfs":measurement["cp_dbfs"],"difference_db":measurement["cp_dbfs"].as_f64().unwrap()-result.integrated_dbfs}));
    for fs in [1_024_000.0,2_400_000.0]{for(mode,bw)in [("AM",12000.0),("NFM",25000.0),("WFM",200000.0)]{for side in [-1.0,1.0]{
        let offset=side*(fs*0.45-bw/2.0);let input=analog(fs,offset,(fs*0.25)as usize,mode);let audio=demod(fs,offset,bw,mode,&input);let steady=&audio[3200..];let fraction=audio_fraction(steady,1000.0);
        check(&mut checks,"AM/FM recover 1kHz at both usable-band edges",(audio.len()as i64-8000).abs()<=1&&audio.iter().all(|x|x.is_finite())&&fraction>0.90,
            json!({"fs":fs,"mode":mode,"bandwidth_hz":bw,"offset_hz":offset,"audio_samples":audio.len(),"1khz_power_fraction":fraction}));
    }}}
    for fs in [1_024_000.0,2_400_000.0]{for(mode,bw)in [("AM",12000.0),("NFM",25000.0),("WFM",200000.0)]{
        let mut pass=VfoDemodulator::new(fs,0.0,bw,mode,32000);let channel_rate=pass.channel_rate;
        let settle=tone(fs,0.0,(fs*0.05)as usize,0.2,Complex32::new(0.0,0.0));pass.process(&settle);
        let pass_result=pass.process_with_metrics(&tone(fs,0.0,(fs*0.05)as usize,0.2,Complex32::new(0.0,0.0)),None,0.0);
        let blocker_hz=channel_rate-1000.0;let mut stop=VfoDemodulator::new(fs,0.0,bw,mode,32000);stop.process(&tone(fs,blocker_hz,(fs*0.05)as usize,0.2,Complex32::new(0.0,0.0)));
        let stop_result=stop.process_with_metrics(&tone(fs,blocker_hz,(fs*0.05)as usize,0.2,Complex32::new(0.0,0.0)),None,0.0);
        let rejection=pass_result.rf_dbfs-stop_result.rf_dbfs;
        check(&mut checks,"Channel decimation rejects a blocker that aliases to 1kHz",blocker_hz<fs/2.0&&rejection>40.0,json!({"fs":fs,"mode":mode,"bandwidth_hz":bw,"channel_rate":channel_rate,"blocker_offset_hz":blocker_hz,"pass_rf_dbfs":pass_result.rf_dbfs,"blocker_rf_dbfs":stop_result.rf_dbfs,"rejection_db":rejection}));
    }}
    let fs=1024000.0;let original=analog(fs,0.0,(fs*0.3)as usize,"AM");let raw_audio=demod(fs,0.0,12000.0,"AM",&original);
    let mut processed=original.clone();CenterDcRejector::new(fs).process_in_place(&mut processed);let filtered_audio=demod(fs,0.0,12000.0,"AM",&processed);
    let before=audio_fraction(&raw_audio[6400..],1000.0);let after=audio_fraction(&filtered_audio[6400..],2000.0);
    check(&mut checks,"Centered AM carrier removal has an explicit harmful effect",before>0.9&&after>0.8,json!({"raw_1khz_fraction":before,"dc_rejected_2khz_fraction":after,"interpretation":"Known filter effect: center AM carrier is removed and envelope folds to 2kHz. Detune receiver center or disable DC rejection for a desired centered station; not RF sensitivity improvement."}));
    let bytes:Vec<u8>=(0u16..=255).flat_map(|x|[x as u8,(255-x)as u8]).collect();let decoded=device::decode(&bytes);
    check(&mut checks,"All cu8 component codes round-trip without wrapping",device::encode(&decoded)==bytes&&decoded[0].re==-1.0&&decoded[0].im==1.0,json!({"codes_per_component":256,"rail_complex_magnitude":decoded[0].norm(),"rail_complex_power_dbfs":db(decoded[0].norm_sqr()as f64),"interpretation":"Complex magnitude1 defines0dBFS; simultaneous I/Q rails can be+3.01dBFS."}));
    let encoded=device::encode(&[Complex32::new(1.5,-1.5)]);check(&mut checks,"Explicit synthetic encode overload saturates without modulo wrap",encoded==[255,0],json!({"encoded_bytes":encoded}));
    let mut wav=WavRecorder::new(folder.join("wav-probe"),"electrical",32000,"PCM16").unwrap();wav.start(json!({"wav_audio":"demod","source":"offline_electrical_probe"})).unwrap();wav.write(&[1.25,-1.25,0.5,-0.5]).unwrap();let summary=wav.stop("offline_probe_complete");
    check(&mut checks,"PCM sink exposes saturation counts",summary["clipped_samples"]==2&&summary["samples"]==4&&summary["bytes"]==8,json!({"summary":summary}));
    let success=checks.iter().all(|c|c["ok"]==true);let report=json!({"schema":"electrical-boundary-rust-probe-1","product_library_executed":true,"source_recompiled_into_probe":false,"hardware_or_server_accessed":false,
        "success":success,"checks":checks,"diagnostics":[],"limitations":["Synthetic digital IQ only. No new RF acquisition, ADC calibration, antenna/AFG sweep, absolute dBm/SINAD or analogue compression guarantee.","Real samples at receiver DC are intentionally affected. Negative-frequency RF UI handling is audited separately.","OBW uses equal tails on a linear frequency axis. A signal wrapping across Nyquist can span almost the full axis; circular shortest-arc OBW is not implemented."]});
    let mut target=OpenOptions::new().write(true).create_new(true).open(&out).unwrap();target.write_all(serde_json::to_string_pretty(&report).unwrap().as_bytes()).unwrap();
    println!("{}",json!({"success":success,"checks":report["checks"].as_array().unwrap().len(),"failures":report["checks"].as_array().unwrap().iter().filter(|c|c["ok"]!=true).collect::<Vec<_>>(),"result":out}));
    if !success{std::process::exit(1);}
}
