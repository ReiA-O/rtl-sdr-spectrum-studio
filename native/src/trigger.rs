//! Sample-clock threshold trigger. No receiver, file, wall-clock or sleep calls.
//! Scalar input values represent analysis blocks, so crossings have block-level
//! resolution; dwell/post/rearm boundaries are source sample indices.

use serde_json::{json, Value};

fn nonnegative(value: f64, name: &str) -> Result<f64, String> {
    if !value.is_finite() || value < 0.0 { return Err(format!("{name} must be nonnegative and finite")); }
    Ok(value)
}
fn number(config: &Value, key: &str, default: f64) -> f64 {config.get(key).and_then(Value::as_f64).unwrap_or(default)}
fn metric(value: &str) -> String {
    match value.to_lowercase().as_str() {
        "peak"|"peak_dbfs"|"peak_level" => "peak".into(),
        "power"|"integrated"|"integrated_power"|"channel_power"|"cp_dbfs"|"power_dbfs" => "power".into(),
        other => other.into(),
    }
}

#[derive(Debug)]
struct Condition {
    metric: String,
    threshold: f64,
    hysteresis: f64,
    minimum_samples: u64,
    below: bool,
    high: bool,
    candidate_start: Option<u64>,
    qualified_at: Option<u64>,
}
impl Condition {
    fn reset(&mut self) {self.high=false;self.candidate_start=None;self.qualified_at=None;}
    fn observe(&mut self, value: Option<f64>, start: u64, end: u64) -> bool {
        let value=match value {Some(v) if v.is_finite()=>v,_=>{self.reset();return false;}};
        let tripped=if self.below {value<=self.threshold} else {value>=self.threshold};
        let released=if self.below {value>self.threshold+self.hysteresis} else {value<self.threshold-self.hysteresis};
        if self.high {if released {self.reset();}return self.high;}
        if !tripped {self.candidate_start=None;return false;}
        let candidate=*self.candidate_start.get_or_insert(start);
        let ready=candidate.saturating_add(self.minimum_samples);
        if ready<=end {self.high=true;self.qualified_at=Some(ready);}
        self.high
    }
}

pub struct TriggerController {
    pub sample_rate: f64,
    pub pre_seconds: f64,
    pub post_seconds: f64,
    pub rearm_s: f64,
    pub threshold_db: f64,
    pub metric: String,
    pub mode: String,
    pub logic: String,
    pub armed: bool,
    pub state: String,
    pub trigger_sample: Option<u64>,
    pub start_sample: Option<u64>,
    pub end_sample: Option<u64>,
    pub record_sequence: u64,
    pub last_invalid_metrics: Vec<String>,
    conditions: Vec<Condition>,
    pre_samples: u64,
    post_samples: u64,
    rearm_samples: u64,
    combined: bool,
    last_end: Option<u64>,
    arm_sample: Option<u64>,
    ready_sample: Option<u64>,
    rearm_until: Option<u64>,
    initial_pre_seconds: f64,
    needs_release: bool,
}

impl TriggerController {
    pub fn new(sample_rate: f64, config: Value) -> Result<Self,String> {
        if !sample_rate.is_finite() || sample_rate<=0.0 {return Err("sample_rate must be positive and finite".into());}
        let pre_seconds=nonnegative(number(&config,"pre_seconds",1.0),"pre_seconds")?;
        let post_seconds=nonnegative(number(&config,"post_seconds",2.0),"post_seconds")?;
        let rearm_s=nonnegative(number(&config,"rearm_s",0.2),"rearm_s")?;
        let hysteresis=nonnegative(number(&config,"hysteresis_db",3.0),"hysteresis_db")?;
        let dwell=nonnegative(number(&config,"min_duration_s",0.0),"min_duration_s")?;
        let threshold_db=number(&config,"threshold_db",-55.0);
        if !threshold_db.is_finite(){return Err("threshold_db must be finite".into());}
        let requested_metric=metric(config.get("metric").and_then(Value::as_str).unwrap_or("peak"));
        let mut mode=config.get("mode").and_then(Value::as_str).unwrap_or("one_shot").to_lowercase().replace('-',"_");
        if mode=="oneshot" || mode=="one shot" {mode="one_shot".into();}
        if mode!="one_shot" && mode!="auto" {return Err("trigger mode must be one_shot or auto".into());}
        let logic=config.get("logic").and_then(Value::as_str).unwrap_or("OR").to_uppercase();
        if logic!="AND" && logic!="OR" {return Err("trigger logic must be AND or OR".into());}
        let mut definitions=config["conditions"].as_array().cloned().unwrap_or_default();
        if definitions.is_empty(){definitions.push(json!({"metric":requested_metric,"threshold_db":threshold_db}));}
        let mut conditions=vec![];
        for item in definitions {
            let threshold=item.get("threshold_db").or_else(||item.get("threshold")).and_then(Value::as_f64).unwrap_or(threshold_db);
            if !threshold.is_finite(){return Err("condition threshold must be finite".into());}
            let condition_dwell=nonnegative(number(&item,"min_duration_s",dwell),"min_duration_s")?;
            let condition_hysteresis=nonnegative(number(&item,"hysteresis_db",hysteresis),"hysteresis_db")?;
            let direction=item.get("direction").and_then(Value::as_str).unwrap_or("above").to_lowercase();
            let below=match direction.as_str(){"above"|"rising"=>false,"below"|"falling"=>true,_=>return Err("condition direction must be above or below".into())};
            conditions.push(Condition {metric:metric(item.get("metric").and_then(Value::as_str).unwrap_or(&requested_metric)),
                threshold,hysteresis:condition_hysteresis,minimum_samples:(condition_dwell*sample_rate).ceil() as u64,
                below,high:false,candidate_start:None,qualified_at:None});
        }
        Ok(Self {sample_rate,pre_seconds,post_seconds,rearm_s,threshold_db,metric:requested_metric,mode,logic,
            armed:false,state:"disarmed".into(),trigger_sample:None,start_sample:None,end_sample:None,
            record_sequence:0,last_invalid_metrics:vec![],conditions,
            pre_samples:(pre_seconds*sample_rate).round() as u64,post_samples:(post_seconds*sample_rate).round() as u64,
            rearm_samples:(rearm_s*sample_rate).round() as u64,combined:false,last_end:None,arm_sample:None,
            ready_sample:None,rearm_until:None,initial_pre_seconds:0.0,needs_release:false})
    }
    pub fn recording(&self)->bool{self.state=="recording"}
    fn reset_conditions(&mut self){self.combined=false;for condition in &mut self.conditions{condition.reset();}}
    pub fn arm(&mut self,sample_index:Option<u64>,pre_available:f64)->Result<Value,String>{
        if self.recording() || self.state=="saving" {return Err("finish the current record before arming again".into());}
        nonnegative(pre_available,"prebuffer_seconds")?;
        self.armed=true;self.state=if self.pre_samples>0{"filling"}else{"waiting"}.into();
        self.arm_sample=sample_index;self.initial_pre_seconds=pre_available;
        let remaining=self.pre_samples.saturating_sub((pre_available*self.sample_rate) as u64);
        self.ready_sample=sample_index.map(|start|start.saturating_add(remaining));
        if remaining==0{self.state="waiting".into();}
        self.rearm_until=None;self.last_end=None;self.trigger_sample=None;self.start_sample=None;self.end_sample=None;
        self.last_invalid_metrics.clear();self.needs_release=false;self.reset_conditions();Ok(self.snapshot())
    }
    pub fn disarm(&mut self)->Value{self.armed=false;self.state="disarmed".into();self.reset_conditions();self.snapshot()}
    fn edge(&mut self,values:&Value,start:u64,end:u64)->Option<u64>{
        let mut states=vec![];self.last_invalid_metrics.clear();
        for condition in &mut self.conditions {
            let keys:Vec<&str>=match condition.metric.as_str(){
                "peak"=>vec!["peak","peak_dbfs","peak_level","peak_db"],
                "power"=>vec!["power","integrated_power","channel_power","cp_dbfs","power_dbfs","power_db","integrated_dbfs"],
                other=>vec![other],
            };
            let value=keys.iter().find_map(|key|values.get(*key)).and_then(Value::as_f64);
            if !value.is_some_and(f64::is_finite){self.last_invalid_metrics.push(condition.metric.clone());}
            states.push(condition.observe(value,start,end));
        }
        let combined=if self.logic=="AND"{states.iter().all(|v|*v)}else{states.iter().any(|v|*v)};
        let crossing=combined&&!self.combined;self.combined=combined;
        if !crossing{return None;}
        let times=self.conditions.iter().filter(|c|c.high).filter_map(|c|c.qualified_at);
        let time=if self.logic=="AND"{times.max()}else{times.min()};
        time.map(|sample|sample.max(start))
    }
    fn finish(&mut self)->Value{
        let event=json!({"type":"complete","sample_index":self.end_sample,"end_sample":self.end_sample,
            "record_sequence":self.record_sequence,"reason":"post_trigger_complete","rearm_mode":self.mode});
        self.reset_conditions();
        if self.mode=="auto"{self.state="saving".into();self.needs_release=true;}
        else{self.state="disarmed".into();self.armed=false;}
        event
    }
    /// Call only after record finalization. Auto remains armed while saving.
    pub fn recording_finished(&mut self,sample_index:Option<u64>,success:bool)->Value{
        if !success{return self.disarm();}
        if self.mode!="auto"||self.state!="saving"{return self.snapshot();}
        let finished=sample_index.or(self.last_end).or(self.end_sample).unwrap_or(0).max(self.end_sample.unwrap_or(0));
        self.rearm_until=Some(finished.saturating_add(self.rearm_samples));self.arm_sample=Some(finished);
        self.ready_sample=Some(finished.saturating_add(self.pre_samples));self.state="rearming".into();
        self.reset_conditions();self.snapshot()
    }
    pub fn notify_saved(&mut self,sample_index:Option<u64>,success:bool)->Value{self.recording_finished(sample_index,success)}
    pub fn evaluate(&mut self,values:&Value,start:u64,count:usize)->Result<Vec<Value>,String>{
        if count==0||!self.armed{return Ok(vec![]);}
        let end=start.checked_add(count as u64).ok_or("trigger sample index overflow")?;
        if self.last_end.is_some_and(|last|start<last){return Err("trigger blocks must not overlap or go backwards".into());}
        if self.last_end.is_some_and(|last|start>last){
            self.reset_conditions();
            if !self.recording()&&self.state!="saving"{self.arm_sample=Some(start);self.ready_sample=Some(start.saturating_add(self.pre_samples));}
        }
        self.last_end=Some(end);
        if self.state=="saving"{return Ok(vec![]);}
        if self.arm_sample.is_none(){self.arm_sample=Some(start);self.ready_sample=Some(start.saturating_add(self.pre_samples.saturating_sub((self.initial_pre_seconds*self.sample_rate) as u64)));}
        let mut events=vec![];let mut effective_start=start;
        if !self.recording(){
            if let Some(rearm)=self.rearm_until{effective_start=effective_start.max(rearm);}
            let ready=if let Some(available)=values.get("available_pre_seconds").and_then(Value::as_f64){
                nonnegative(available,"available_pre_seconds")?;
                if available*self.sample_rate>=self.pre_samples as f64{effective_start}
                else{effective_start.max(start.saturating_add(self.pre_samples.saturating_sub((available*self.sample_rate) as u64)))}
            }else{effective_start.max(self.ready_sample.unwrap_or(start))};
            if ready>=end{self.state=if self.rearm_until.is_some(){"rearming"}else{"filling"}.into();self.reset_conditions();return Ok(vec![]);}
            effective_start=ready;
            if self.rearm_until.is_some(){events.push(json!({"type":"rearmed","sample_index":effective_start}));self.rearm_until=None;}
            self.state="waiting".into();
        }
        let crossing=self.edge(values,effective_start,end);
        if !self.recording()&&self.needs_release{
            if !self.combined{self.needs_release=false;}
            return Ok(events);
        }
        if self.recording(){
            if let Some(at)=crossing.filter(|at|*at<=self.end_sample.unwrap()){
                self.end_sample=Some(self.end_sample.unwrap().max(at.saturating_add(self.post_samples)));
                events.push(json!({"type":"extend","sample_index":at,"end_sample":self.end_sample,"record_sequence":self.record_sequence}));
            }
            if self.end_sample.unwrap()<=end{events.push(self.finish());}
            return Ok(events);
        }
        if let Some(at)=crossing{
            self.record_sequence=self.record_sequence.saturating_add(1);self.trigger_sample=Some(at);
            self.start_sample=Some(at.saturating_sub(self.pre_samples));self.end_sample=Some(at.saturating_add(self.post_samples));self.state="recording".into();
            events.push(json!({"type":"trigger","sample_index":at,"trigger_sample":at,"start_sample":self.start_sample,
                "end_sample":self.end_sample,"record_sequence":self.record_sequence,"pre_seconds":self.pre_seconds,"post_seconds":self.post_seconds}));
            if self.end_sample.unwrap()<=end{events.push(self.finish());}
        }
        Ok(events)
    }
    pub fn snapshot(&self)->Value{
        let conditions:Vec<Value>=self.conditions.iter().map(|c|json!({"metric":c.metric,"threshold_db":c.threshold,
            "hysteresis_db":c.hysteresis,"min_duration_s":c.minimum_samples as f64/self.sample_rate,
            "direction":if c.below{"below"}else{"above"},"active":c.high})).collect();
        json!({"armed":self.armed,"state":self.state,"recording":self.recording(),"mode":self.mode,"logic":self.logic,
            "pre_seconds":self.pre_seconds,"post_seconds":self.post_seconds,"rearm_s":self.rearm_s,
            "trigger_sample":self.trigger_sample,"start_sample":self.start_sample,"end_sample":self.end_sample,
            "record_sequence":self.record_sequence,"last_invalid_metrics":self.last_invalid_metrics,
            "waiting_for_release":self.needs_release,"conditions":conditions})
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn dwell_hysteresis_retrigger_and_one_shot(){
        let mut t=TriggerController::new(100.0,json!({"pre_seconds":0,"post_seconds":0.5,"threshold_db":-50,"hysteresis_db":3,"min_duration_s":0.1})).unwrap();
        t.arm(None,0.0).unwrap();assert!(t.evaluate(&json!({"peak":-40}),0,5).unwrap().is_empty());
        let e=t.evaluate(&json!({"peak":-40}),5,5).unwrap();assert_eq!(e[0]["sample_index"],10);assert_eq!(t.end_sample,Some(60));
        assert!(t.evaluate(&json!({"peak":-51}),10,10).unwrap().is_empty());
        t.evaluate(&json!({"peak":-54}),20,10).unwrap();let e=t.evaluate(&json!({"peak":-40}),30,10).unwrap();
        assert_eq!(e[0]["type"],"extend");assert_eq!(t.end_sample,Some(90));assert_eq!(t.record_sequence,1);
        let e=t.evaluate(&json!({"peak":-60}),40,50).unwrap();assert_eq!(e[0]["sample_index"],90);assert!(!t.armed);
    }
    #[test]
    fn boundary_retrigger_wins(){
        let mut t=TriggerController::new(100.0,json!({"pre_seconds":0,"post_seconds":0.3,"min_duration_s":0.05})).unwrap();
        t.arm(None,0.0).unwrap();t.evaluate(&json!({"peak":-40}),0,5).unwrap();t.evaluate(&json!({"peak":-100}),5,25).unwrap();
        let e=t.evaluate(&json!({"peak":-40}),30,10).unwrap();assert_eq!(e[0]["type"],"extend");assert_eq!(e[0]["sample_index"],35);assert_eq!(t.end_sample,Some(65));
    }
    #[test]
    fn auto_waits_for_save_deadtime_and_release(){
        let mut t=TriggerController::new(100.0,json!({"pre_seconds":0,"post_seconds":0.1,"mode":"auto","rearm_s":0.1})).unwrap();
        t.arm(None,0.0).unwrap();let e=t.evaluate(&json!({"peak":-40}),0,10).unwrap();assert_eq!(e.len(),2);assert_eq!(t.state,"saving");assert!(t.armed);
        assert!(t.evaluate(&json!({"peak":-100}),10,10).unwrap().is_empty());t.recording_finished(Some(20),true);
        assert!(t.evaluate(&json!({"peak":-40}),20,10).unwrap().is_empty());let e=t.evaluate(&json!({"peak":-40}),30,10).unwrap();assert_eq!(e.len(),1);assert_eq!(e[0]["type"],"rearmed");
        t.evaluate(&json!({"peak":-100}),40,10).unwrap();let e=t.evaluate(&json!({"peak":-40}),50,10).unwrap();assert_eq!(e[0]["type"],"trigger");assert_eq!(t.record_sequence,2);
    }
    #[test]
    fn prebuffer_and_and_or(){
        let mut t=TriggerController::new(10.0,json!({"pre_seconds":1,"post_seconds":1})).unwrap();t.arm(None,0.0).unwrap();
        assert!(t.evaluate(&json!({"peak":-40}),0,5).unwrap().is_empty());assert!(t.evaluate(&json!({"peak":-40}),5,5).unwrap().is_empty());
        let e=t.evaluate(&json!({"peak":-40}),10,5).unwrap();assert_eq!(e[0]["start_sample"],0);
        let mut and=TriggerController::new(100.0,json!({"pre_seconds":0,"logic":"AND","conditions":[{"metric":"peak","threshold_db":-50},{"metric":"power","threshold_db":-60}]})).unwrap();
        and.arm(None,0.0).unwrap();assert!(and.evaluate(&json!({"peak":-40,"power":-70}),0,10).unwrap().is_empty());
        assert_eq!(and.evaluate(&json!({"peak":-40,"power":-50}),10,10).unwrap()[0]["type"],"trigger");
    }
    #[test]
    fn gaps_do_not_satisfy_dwell_and_failure_disarms(){
        let mut t=TriggerController::new(100.0,json!({"pre_seconds":0,"min_duration_s":0.1,"mode":"auto"})).unwrap();t.arm(None,0.0).unwrap();
        assert!(t.evaluate(&json!({"peak":-40}),0,5).unwrap().is_empty());assert!(t.evaluate(&json!({"peak":-40}),10,5).unwrap().is_empty());
        assert_eq!(t.evaluate(&json!({"peak":-40}),15,5).unwrap()[0]["sample_index"],20);
        t.recording_finished(None,false);assert!(!t.armed);assert_eq!(t.state,"disarmed");
        assert!(TriggerController::new(100.0,json!({"pre_seconds":-1})).is_err());
    }
}
