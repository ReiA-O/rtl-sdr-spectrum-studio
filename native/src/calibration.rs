//! User-supplied flat-band level correction. This checks applicability; it does
//! not measure a correction, certify RF accuracy, or alter stored samples/PSD.
use chrono::{DateTime,Utc};
use serde_json::{json,Value};

#[derive(Clone,Debug)]
pub struct Calibration {
    data:Value,
    pub offset_db:f64,
    pub label:String,
    low:f64,high:f64,sample_rate:f64,gain_db:f64,mode:String,device_name:String,
    expires:DateTime<Utc>,
}
fn number(value:&Value,key:&str)->Result<f64,String>{
    value[key].as_f64().filter(|n|n.is_finite()).ok_or_else(||format!("校正の {key} は有限数が必要です"))
}
fn required(value:&Value,key:&str,max:usize)->Result<String,String>{
    value[key].as_str().map(str::trim).filter(|s|!s.is_empty()&&s.chars().count()<=max)
        .map(str::to_owned).ok_or_else(||format!("校正の {key} は1〜{max}文字で指定してください"))
}
fn matches(a:f64,b:f64)->bool{a.is_finite()&&b.is_finite()&&(a-b).abs()<=1e-6*a.abs().max(b.abs()).max(1.0)}
impl Calibration {
    /// Structural validation also permits expired saved profiles, so they can
    /// remain visible as invalid rather than corrupting unrelated settings.
    pub fn from_json(value:&Value)->Result<Self,String>{
        if !value.is_object()||value["schema"]!="spectrum-studio-calibration-1"{return Err("校正のschemaは spectrum-studio-calibration-1 です".into());}
        let offset=number(value,"offset_db")?;let low=number(value,"frequency_low_hz")?;let high=number(value,"frequency_high_hz")?;
        let rate=number(value,"sample_rate")?;let gain=number(value,"gain_db")?;
        if low<=0.0||high<=low||high>1_766_000_000.0{return Err("校正周波数範囲は0 Hz超〜1.766 GHz以内で下端＜上端にしてください".into());}
        if rate<=0.0||rate>10_000_000.0{return Err("校正サンプルレートは0超〜10 MS/s以内です".into());}
        if !(-10.0..=60.0).contains(&gain){return Err("校正ゲインは-10〜60 dB以内です".into());}
        let mode=required(value,"mode",20)?;if !["tuner","direct_i","direct_q"].contains(&mode.as_str()){return Err("校正入力モードは tuner/direct_i/direct_q です".into());}
        if value["agc"].as_bool()!=Some(false){return Err("絶対レベル校正はAGC OFFの場合だけ使用できます".into());}
        let label=required(value,"label",200)?;let name=required(value,"device_name",200)?;required(value,"reference_note",4000)?;
        let expiry=required(value,"expires_utc",100)?;
        let expires=DateTime::parse_from_rfc3339(&expiry).map_err(|_|"校正 expires_utc はRFC3339日時が必要です")?.with_timezone(&Utc);
        Ok(Self{data:value.clone(),offset_db:offset,label,low,high,sample_rate:rate,gain_db:gain,mode,device_name:name,expires})
    }
    pub fn load_at(value:&Value,now:DateTime<Utc>)->Result<Self,String>{
        let calibration=Self::from_json(value)?;if now>=calibration.expires{return Err("校正の有効期限が切れています".into());}Ok(calibration)
    }
    pub fn to_json(&self)->Value{self.data.clone()}
    /// `metadata` is the actual live-source readback, never restored settings.
    pub fn check_at(&self,source:&str,running:bool,input:&Value,metadata:&Value,usable_hz:f64,measurement_band:(f64,f64),now:DateTime<Utc>)->Result<(),String>{
        if now>=self.expires{return Err("校正の有効期限が切れています".into());}
        if source!="live"||!running||metadata["synthetic"].as_bool()==Some(true){return Err("実機のライブ受信と実効入力条件を確認できる場合だけ有効です".into());}
        if input["agc"].as_bool()!=Some(false)||metadata["agc"].as_bool()!=Some(false){return Err("AGC OFFの実効状態が一致しません".into());}
        let actual_rate=metadata["sample_rate"].as_f64().ok_or("実効サンプルレートが未確認です")?;
        let requested_rate=input["sample_rate"].as_f64().ok_or("現在のサンプルレートが未確認です")?;
        if !matches(actual_rate,self.sample_rate)||!matches(actual_rate,requested_rate){return Err("校正時と実効サンプルレートが一致しません".into());}
        let gain=metadata["gain_db_actual"].as_f64().ok_or("実効ゲインが未確認のため絶対単位を使用できません")?;
        if !matches(gain,self.gain_db)||!input["gain_actual_db"].as_f64().is_some_and(|current|matches(current,gain)){return Err("校正時と実効ゲインが一致しません".into());}
        if metadata["mode_actual"].as_str()!=Some(self.mode.as_str()){return Err("校正時と実効入力モードが一致しません".into());}
        if metadata["name"].as_str()!=Some(self.device_name.as_str()){return Err("校正時と実機名が一致しません".into());}
        let center=input["center_hz"].as_f64().ok_or("中心周波数が未確認です")?;
        if !metadata["center_hz"].as_f64().is_some_and(|actual|matches(actual,center)){return Err("中心周波数の実効状態が一致しません".into());}
        if !usable_hz.is_finite()||usable_hz<=0.0||center-usable_hz/2.0<self.low||center+usable_hz/2.0>self.high{return Err("取得有効帯域の全幅が校正範囲内にありません".into());}
        if !measurement_band.0.is_finite()||!measurement_band.1.is_finite()||measurement_band.0>=measurement_band.1||measurement_band.0<self.low||measurement_band.1>self.high{return Err("測定帯域が校正範囲内にありません".into());}
        Ok(())
    }
    pub fn contains(&self,frequency:f64)->bool{frequency.is_finite()&&frequency>=self.low&&frequency<=self.high}
}

/// Apply a display-only offset. Relative measurement fields are retained for
/// traceability; image bytes and histogram counts remain exactly unchanged.
pub fn apply_frame(frame:&mut Value,offset:Option<f64>,range:Option<&Calibration>){
    let unit=if offset.is_some(){"dBm"}else{"dBFS"};
    if let Some(spectrum)=frame["spectrum"].as_object_mut(){
        if let Some(offset)=offset{for(key,value)in spectrum.iter_mut(){if key.ends_with("_db"){if let Some(values)=value.as_array_mut(){for item in values{if let Some(level)=item.as_f64(){*item=json!(level+offset);}}}}}}
        spectrum.insert("unit".into(),json!(unit));
    }
    for name in ["density","waterfall"]{if let Some(object)=frame[name].as_object_mut(){
        if let Some(offset)=offset{for key in ["min_db","max_db"]{if let Some(level)=object.get(key).and_then(Value::as_f64){object.insert(key.into(),json!(level+offset));}}}
        object.insert("unit".into(),json!(unit));
    }}
    if let Some(measurement)=frame["measurements"].as_object_mut(){
        for(relative,absolute)in [("cp_dbfs","cp_dbm"),("cp_avg_dbfs","cp_avg_dbm")]{
            let converted=offset.and_then(|o|measurement.get(relative).and_then(Value::as_f64).map(|level|level+o));measurement.insert(absolute.into(),json!(converted));
        }
        if let Some(markers)=measurement.get_mut("markers").and_then(Value::as_array_mut){for marker in markers{
            let converted=offset.filter(|_|range.is_none_or(|cal|marker["frequency_hz"].as_f64().is_some_and(|f|cal.contains(f))))
                .and_then(|o|marker["level_dbfs"].as_f64().map(|level|level+o));marker["level_dbm"]=json!(converted);
        }}
        measurement.insert("unit".into(),json!(unit));
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    fn now()->DateTime<Utc>{DateTime::parse_from_rfc3339("2026-10-27T00:00:00Z").unwrap().with_timezone(&Utc)}
    fn profile()->Value{json!({"schema":"spectrum-studio-calibration-1","label":"test reference","offset_db":-35.0,
        "frequency_low_hz":30000000.0,"frequency_high_hz":35000000.0,"sample_rate":2048000.0,"gain_db":20.7,
        "mode":"tuner","agc":false,"device_name":"Test SDR","expires_utc":"2027-01-01T00:00:00Z","reference_note":"test only, measured reference required"})}
    fn conditions()->(Value,Value){(json!({"center_hz":32256000.0,"sample_rate":2048000.0,"gain_actual_db":20.7,"agc":false}),
        json!({"name":"Test SDR","center_hz":32256000.0,"sample_rate":2048000.0,"gain_db_actual":20.7,"agc":false,"mode_actual":"tuner"}))}
    #[test]
    fn invalid_structure_and_expired_profiles_are_rejected(){
        let mut value=profile();value["expires_utc"]=json!("2025-01-01T00:00:00Z");assert!(Calibration::load_at(&value,now()).is_err());assert!(Calibration::from_json(&value).is_ok());
        value=profile();value["frequency_low_hz"]=json!(0);assert!(Calibration::from_json(&value).is_err());
        value=profile();value["reference_note"]=json!("");assert!(Calibration::from_json(&value).is_err());
        value=profile();value["agc"]=json!(true);assert!(Calibration::from_json(&value).is_err());
    }
    #[test]
    fn actual_source_conditions_and_full_band_gate_the_correction(){
        let calibration=Calibration::load_at(&profile(),now()).unwrap();let(input,metadata)=conditions();let band=(31900000.0,32100000.0);
        assert!(calibration.check_at("live",true,&input,&metadata,1843200.0,band,now()).is_ok());
        for source in ["demo","replay"]{assert!(calibration.check_at(source,true,&input,&metadata,1843200.0,band,now()).is_err());}
        assert!(calibration.check_at("live",false,&input,&metadata,1843200.0,band,now()).is_err());
        let mut wrong=metadata.clone();wrong["gain_db_actual"]=json!(28.0);assert!(calibration.check_at("live",true,&input,&wrong,1843200.0,band,now()).is_err());
        wrong=metadata.clone();wrong["gain_db_actual"]=Value::Null;assert!(calibration.check_at("live",true,&input,&wrong,1843200.0,band,now()).is_err());
        wrong=metadata.clone();wrong["name"]=json!("Other SDR");assert!(calibration.check_at("live",true,&input,&wrong,1843200.0,band,now()).is_err());
        assert!(calibration.check_at("live",true,&input,&metadata,6000000.0,band,now()).is_err());
        assert!(calibration.check_at("live",true,&input,&metadata,1843200.0,(29000000.0,32100000.0),now()).is_err());
        assert!(calibration.check_at("live",true,&input,&metadata,1843200.0,band,now()+chrono::Duration::days(100)).is_err());
    }
    #[test]
    fn display_transform_preserves_raw_measurements_obw_and_image_bytes(){
        let mut frame=json!({"spectrum":{"live_db":[-60.0,-50.0],"max_db":[-40.0],"f_hz":[32000000]},"density":{"min_db":-140,"max_db":0,"data_b64":"ABCD","valid_frames":10},
            "waterfall":{"min_db":-120,"max_db":-20,"data_b64":"EFGH"},"measurements":{"cp_dbfs":-45,"cp_avg_dbfs":-46,"obw_hz":12345,"markers":[{"frequency_hz":32000000,"level_dbfs":-50,"delta_db":3}]}});
        let original=frame.clone();let calibration=Calibration::from_json(&profile()).unwrap();apply_frame(&mut frame,Some(-35.0),Some(&calibration));
        assert_eq!(frame["spectrum"]["live_db"][0],-95.0);assert_eq!(frame["spectrum"]["unit"],"dBm");
        assert_eq!(frame["measurements"]["cp_dbfs"],original["measurements"]["cp_dbfs"]);assert_eq!(frame["measurements"]["cp_dbm"],-80.0);
        assert_eq!(frame["measurements"]["markers"][0]["level_dbm"],-85.0);assert_eq!(frame["measurements"]["obw_hz"],12345);
        assert_eq!(frame["density"]["data_b64"],"ABCD");assert_eq!(frame["density"]["valid_frames"],10);assert_eq!(frame["waterfall"]["data_b64"],"EFGH");
    }
}
