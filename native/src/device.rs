use libloading::Library;
use rustfft::num_complex::Complex32;
use serde_json::{Value,json};
use std::{ffi::{c_void,c_char,CStr},path::{Path,PathBuf},sync::{Arc,atomic::{AtomicBool,AtomicU64,Ordering},mpsc::{self,Receiver,SyncSender,TrySendError}},thread::{self,JoinHandle},time::{Duration,Instant}};

pub const BLOCK_SAMPLES: usize = 131072;
pub struct Block { pub start: u64, pub raw: Vec<u8> }
type Dev = *mut c_void;
type Callback = unsafe extern "C" fn(*mut u8,u32,*mut c_void);

pub struct RtlLibrary {
    _library: Library,
    count: unsafe extern "C" fn()->u32,
    name: unsafe extern "C" fn(u32)->*const c_char,
    open: unsafe extern "C" fn(*mut Dev,u32)->i32,
    close: unsafe extern "C" fn(Dev)->i32,
    set_rate: unsafe extern "C" fn(Dev,u32)->i32,
    get_rate: unsafe extern "C" fn(Dev)->u32,
    set_center: unsafe extern "C" fn(Dev,u32)->i32,
    get_center: unsafe extern "C" fn(Dev)->u32,
    set_gain_mode: unsafe extern "C" fn(Dev,i32)->i32,
    set_gain: unsafe extern "C" fn(Dev,i32)->i32,
    get_gain: unsafe extern "C" fn(Dev)->i32,
    gains: unsafe extern "C" fn(Dev,*mut i32)->i32,
    set_ppm: unsafe extern "C" fn(Dev,i32)->i32,
    set_direct: unsafe extern "C" fn(Dev,i32)->i32,
    get_direct: unsafe extern "C" fn(Dev)->i32,
    set_agc: unsafe extern "C" fn(Dev,i32)->i32,
    bias: unsafe extern "C" fn(Dev,i32)->i32,
    eeprom: unsafe extern "C" fn(Dev,*mut u8,u8,u16)->i32,
    reset: unsafe extern "C" fn(Dev)->i32,
    read_async: unsafe extern "C" fn(Dev,Callback,*mut c_void,u32,u32)->i32,
    cancel: unsafe extern "C" fn(Dev)->i32,
}

impl RtlLibrary {
    pub fn load(path:&Path)->Result<Arc<Self>,String> {
        unsafe {
            #[cfg(windows)]
            let library:Library = libloading::os::windows::Library::load_with_flags(path,0x1100).map_err(|e|format!("受信DLLを開けません: {e}"))?.into();
            #[cfg(not(windows))]
            let library=Library::new(path).map_err(|e|e.to_string())?;
            macro_rules! sym {($name:literal,$ty:ty)=>{*library.get::<$ty>(concat!($name,"\0").as_bytes()).map_err(|e|e.to_string())?};}
            Ok(Arc::new(Self {
                count:sym!("rtlsdr_get_device_count",unsafe extern "C" fn()->u32),
                name:sym!("rtlsdr_get_device_name",unsafe extern "C" fn(u32)->*const c_char),
                open:sym!("rtlsdr_open",unsafe extern "C" fn(*mut Dev,u32)->i32),
                close:sym!("rtlsdr_close",unsafe extern "C" fn(Dev)->i32),
                set_rate:sym!("rtlsdr_set_sample_rate",unsafe extern "C" fn(Dev,u32)->i32),
                get_rate:sym!("rtlsdr_get_sample_rate",unsafe extern "C" fn(Dev)->u32),
                set_center:sym!("rtlsdr_set_center_freq",unsafe extern "C" fn(Dev,u32)->i32),
                get_center:sym!("rtlsdr_get_center_freq",unsafe extern "C" fn(Dev)->u32),
                set_gain_mode:sym!("rtlsdr_set_tuner_gain_mode",unsafe extern "C" fn(Dev,i32)->i32),
                set_gain:sym!("rtlsdr_set_tuner_gain",unsafe extern "C" fn(Dev,i32)->i32),
                get_gain:sym!("rtlsdr_get_tuner_gain",unsafe extern "C" fn(Dev)->i32),
                gains:sym!("rtlsdr_get_tuner_gains",unsafe extern "C" fn(Dev,*mut i32)->i32),
                set_ppm:sym!("rtlsdr_set_freq_correction",unsafe extern "C" fn(Dev,i32)->i32),
                set_direct:sym!("rtlsdr_set_direct_sampling",unsafe extern "C" fn(Dev,i32)->i32),
                get_direct:sym!("rtlsdr_get_direct_sampling",unsafe extern "C" fn(Dev)->i32),
                set_agc:sym!("rtlsdr_set_agc_mode",unsafe extern "C" fn(Dev,i32)->i32),
                bias:sym!("rtlsdr_set_bias_tee",unsafe extern "C" fn(Dev,i32)->i32),
                eeprom:sym!("rtlsdr_read_eeprom",unsafe extern "C" fn(Dev,*mut u8,u8,u16)->i32),
                reset:sym!("rtlsdr_reset_buffer",unsafe extern "C" fn(Dev)->i32),
                read_async:sym!("rtlsdr_read_async",unsafe extern "C" fn(Dev,Callback,*mut c_void,u32,u32)->i32),
                cancel:sym!("rtlsdr_cancel_async",unsafe extern "C" fn(Dev)->i32),
                _library:library,
            }))
        }
    }
    pub fn devices(&self)->Vec<Value> {
        unsafe {(0..(self.count)()).map(|i|{
            let p=(self.name)(i);let name=if p.is_null(){"RTL-SDR".to_owned()}else{CStr::from_ptr(p).to_string_lossy().into_owned()};
            json!({"index":i,"name":name})
        }).collect()}
    }
}

fn check(code:i32,label:&str)->Result<(),String>{if code==0{Ok(())}else{Err(format!("{label}に失敗しました（RTL-SDR {code}）。"))}}
fn device_index(config:&Value)->Result<u32,String>{let index=match config.get("device_index"){None=>0,Some(v)=>v.as_u64().ok_or("受信機番号が不正です。")?};u32::try_from(index).map_err(|_|"受信機番号が大きすぎます。".into())}
fn correction_ppm(config:&Value)->Result<i32,String>{let ppm=config.get("ppm").and_then(Value::as_f64).unwrap_or(0.);if !ppm.is_finite()||ppm.fract()!=0.||ppm.abs()>500.{return Err("PPMは-500〜500の整数です。".into())}Ok(ppm as i32)}
struct Context {sender:SyncSender<Block>,index:AtomicU64,pub dropped:AtomicU64,stop:Arc<AtomicBool>}
unsafe extern "C" fn callback(ptr:*mut u8,len:u32,ctx:*mut c_void) {
    if ctx.is_null() || ptr.is_null() || len%2!=0{return}
    let c=unsafe{&*(ctx as *const Context)};
    let start=c.index.fetch_add((len/2)as u64,Ordering::Relaxed);
    if c.stop.load(Ordering::Relaxed){return}
    let raw=unsafe{std::slice::from_raw_parts(ptr,len as usize)}.to_vec();
    if let Err(TrySendError::Full(block))=c.sender.try_send(Block{start,raw}){c.dropped.fetch_add((block.raw.len()/2)as u64,Ordering::Relaxed);}
}

pub struct RtlSource {lib:Arc<RtlLibrary>,dev:usize,rx:Receiver<Block>,context:Option<Box<Context>>,pump:Option<JoinHandle<i32>>,pub stop:Arc<AtomicBool>,pub metadata:Value}
impl RtlSource {
    pub fn open(lib:Arc<RtlLibrary>,config:&Value,stop:Arc<AtomicBool>)->Result<Self,String> {
        let devices=lib.devices();let index=device_index(config)?;
        let name=devices.iter().find(|d|d["index"].as_u64()==Some(index as u64)).ok_or("RTL-SDRが見つかりません。USB接続とドライバーを確認してください。")?["name"].clone();
        let mut dev:Dev=std::ptr::null_mut();unsafe{check((lib.open)(&mut dev,index),"接続")?;}
        let setup=(||unsafe{
            let mut buf=[0u8;16];let e=(lib.eeprom)(dev,buf.as_mut_ptr(),0,16);
            if e<0 || buf[..2]!=[0x28,0x32]{return Err("Bias-T強制設定を確認できません。".to_owned())}
            if buf[7]&0x02==0{return Err("Bias-T強制ONの機器はこの構成で開始できません。".to_owned())}
            check((lib.bias)(dev,0),"Bias-T OFF要求")?;
            let center=config["center_hz"].as_f64().unwrap_or(32_256_000.)as u32;
            let fs=config["sample_rate"].as_f64().unwrap_or(2_048_000.)as u32;
            let mode=config["mode"].as_str().unwrap_or("auto");
            let direct=if mode=="direct_i"{1}else if mode=="direct_q" ||(mode=="auto"&&center<24_000_000){2}else{0};
            check((lib.set_direct)(dev,direct),"入力モード")?;
            check((lib.set_rate)(dev,fs),"サンプルレート")?;
            let ppm=correction_ppm(config)?;let ppm_rc=(lib.set_ppm)(dev,ppm);if ppm_rc!=0&&ppm_rc!=-2{check(ppm_rc,"PPM補正")?;}
            check((lib.set_center)(dev,center),"中心周波数")?;
            let agc=config["agc"].as_bool().unwrap_or(false);check((lib.set_agc)(dev,if agc{1}else{0}),"AGC")?;
            let gain=config["gain_db"].as_f64().unwrap_or(20.7);let mut gains=Vec::<i32>::new();
            if direct==0 {
                check((lib.set_gain_mode)(dev,if agc{0}else{1}),"ゲインモード")?;
                let n=(lib.gains)(dev,std::ptr::null_mut());if n>0&&n<256{gains.resize(n as usize,0);(lib.gains)(dev,gains.as_mut_ptr());}
                if !agc{if let Some(g)=gains.iter().min_by(|a,b|((**a as f64/10.-gain).abs()).total_cmp(&(**b as f64/10.-gain).abs())){check((lib.set_gain)(dev,*g),"ゲイン")?;}}
            }
            check((lib.reset)(dev),"受信バッファ")?;
            Ok(json!({"name":name,"device_index":index,"center_hz":(lib.get_center)(dev),"sample_rate":(lib.get_rate)(dev),
                "gain_db_requested":gain,"gain_db_actual":if direct==0&&!agc{Some((lib.get_gain)(dev)as f64/10.)}else{None},
                "available_gains_db":gains.iter().map(|g|*g as f64/10.).collect::<Vec<_>>(),"agc":agc,"ppm":ppm,
                "mode_requested":mode,"mode_actual":match (lib.get_direct)(dev){1=>"direct_i",2=>"direct_q",_=>"tuner"},
                "bias_tee_off_requested":true,"bias_tee_voltage_measured":false,"eeprom_written":false}))
        })();
        let mut metadata=match setup{Ok(v)=>v,Err(e)=>{unsafe{(lib.close)(dev);};return Err(e)}};
        metadata["stream_start_utc"]=json!(chrono::Utc::now().to_rfc3339());
        metadata["stream_clock_accuracy"]=json!("host_estimate_before_async_start_not_calibrated");
        let(tx,rx)=mpsc::sync_channel(24);let mut context=Box::new(Context{sender:tx,index:AtomicU64::new(0),dropped:AtomicU64::new(0),stop:stop.clone()});
        let context_ptr=(&mut *context)as *mut Context as usize;let handle=dev as usize;let runner=lib.clone();let signal=stop.clone();
        let pump=match thread::Builder::new().name("rtl-usb".into()).spawn(move||{if signal.load(Ordering::Acquire){return 0}unsafe{(runner.read_async)(handle as Dev,callback,context_ptr as *mut c_void,12,(BLOCK_SAMPLES*2)as u32)}}){Ok(p)=>p,Err(e)=>{unsafe{(lib.close)(dev);};return Err(e.to_string())}};
        Ok(Self{lib,dev:handle,rx,context:Some(context),pump:Some(pump),stop,metadata})
    }
    pub fn read(&self)->Result<Option<Block>,String>{match self.rx.recv_timeout(Duration::from_millis(250)){
        Ok(b)=>Ok(Some(b)),Err(mpsc::RecvTimeoutError::Timeout)=>{if self.pump.as_ref().is_some_and(|h|h.is_finished())&&!self.stop.load(Ordering::Acquire){Err("USB受信が停止しました。再接続後に受信開始を押してください。".into())}else{Ok(None)}},Err(_)=>Err("USB接続が終了しました。".into())}}
    pub fn close(&mut self)->Result<(),String>{
        self.stop.store(true,Ordering::Release);
        if self.dev==0{return Ok(())}
        for _ in 0..40{unsafe{(self.lib.cancel)(self.dev as Dev);};if self.pump.as_ref().is_none_or(|p|p.is_finished()){break}thread::sleep(Duration::from_millis(50));}
        if self.pump.as_ref().is_some_and(|p|!p.is_finished()){return Err("USB停止処理を待っています。機器を使用中のまま再設定しないでください。".into())}
        if let Some(p)=self.pump.take(){let _=p.join();}
        unsafe{(self.lib.close)(self.dev as Dev);};self.dev=0;self.context.take();Ok(())
    }
}
impl Drop for RtlSource{fn drop(&mut self){if self.close().is_err(){if let Some(c)=self.context.take(){std::mem::forget(c);}if let Some(p)=self.pump.take(){std::mem::forget(p);}}}}

pub fn decode(raw:&[u8])->Vec<Complex32>{raw.chunks_exact(2).map(|p|Complex32::new((p[0]as f32-127.5)/127.5,(p[1]as f32-127.5)/127.5)).collect()}
pub fn encode(values:&[Complex32])->Vec<u8>{let mut raw=Vec::with_capacity(values.len()*2);for v in values{raw.push((v.re*127.5+127.5).round().clamp(0.,255.)as u8);raw.push((v.im*127.5+127.5).round().clamp(0.,255.)as u8);}raw}
pub struct DemoSource{fs:f64,offset:f64,index:u64,deadline:Instant,rng:u64}
impl DemoSource{
    pub fn new(config:&Value)->Self{Self{fs:config["sample_rate"].as_f64().unwrap_or(2_048_000.),offset:-256_000.,index:0,deadline:Instant::now(),rng:731}}
    fn noise(&mut self)->f64{self.rng^=self.rng<<13;self.rng^=self.rng>>7;self.rng^=self.rng<<17;(self.rng as f64/u64::MAX as f64-0.5)*0.015}
    pub fn read(&mut self,stop:&AtomicBool)->Option<Block>{
        self.deadline+=Duration::from_secs_f64(BLOCK_SAMPLES as f64/self.fs);
        while Instant::now()<self.deadline{if stop.load(Ordering::Acquire){return None}thread::sleep(Duration::from_millis(1));}
        let start=self.index;let mut values=Vec::with_capacity(BLOCK_SAMPLES);
        for n in 0..BLOCK_SAMPLES{let t=(start+n as u64)as f64/self.fs;let p=std::f64::consts::TAU*self.offset*t+75.*(std::f64::consts::TAU*1000.*t).sin();let p2=std::f64::consts::TAU*350_000.*t;let a=0.05*(1.+0.5*(std::f64::consts::TAU*700.*t).cos());values.push(Complex32::new((0.15*p.cos()+a*p2.cos()+self.noise())as f32,(0.15*p.sin()+a*p2.sin()+self.noise())as f32));}
        self.index+=BLOCK_SAMPLES as u64;Some(Block{start,raw:encode(&values)})
    }
}

pub fn dll_path(project:&Path)->PathBuf {
    std::env::var_os("STUDIO_RTLSDR_DLL").map(PathBuf::from).unwrap_or_else(||{
        let portable=project.join("drivers/rtlsdr/rtlsdr.dll");
        if portable.is_file(){portable}else{project.join("tools/driver-setup-20261026/rtl-sdr-blog-V1.4.0/x64/rtlsdr.dll")}
    })
}

#[cfg(test)]mod tests{use super::*;#[test]fn integer_numeric_configuration_does_not_silently_revert(){assert_eq!(correction_ppm(&json!({"ppm":10.0})).unwrap(),10);assert_eq!(correction_ppm(&json!({"ppm":-12})).unwrap(),-12);assert!(correction_ppm(&json!({"ppm":0.5})).is_err());assert_eq!(device_index(&json!({})).unwrap(),0);assert!(device_index(&json!({"device_index":4294967296u64})).is_err());}}
