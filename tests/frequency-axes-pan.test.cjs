'use strict';
// Real UI functions with deterministic DOM/action doubles. No USB, browser,
// instrument, HTTP server or speakers are used by these checks.
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const test=require('node:test'),assert=require('node:assert/strict');
const source=fs.readFileSync(path.join(__dirname,'../web/app.js'),'utf8');
function functionSource(name){
  const match=new RegExp(`^  (?:async )?function ${name}\\(`,'m').exec(source);assert(match,`${name} exists`);
  const opening=source.indexOf('{',source.indexOf(')',match.index));let depth=0;
  for(let i=opening;i<source.length;i++){if(source[i]==='{')depth++;if(source[i]==='}'&&--depth===0)return source.slice(match.index,i+1);}
  throw new Error(`Unterminated ${name}`);
}
const names=['frequencyViewport','plotGeometry','heatmapFrequencyBounds','frequencyPanContext','frequencyPanPolicy','frequencyPanPlan','beginFrequencyPan','moveFrequencyPan','cancelFrequencyPan','finishFrequencyPan','commitFrequencyPan','frequencyPanKey','centeredVfoPlan','addCenteredVfo','applyState'];
const script=new vm.Script(names.map(functionSource).join('\n'));
const clone=x=>JSON.parse(JSON.stringify(x));
function fixture(){return {running:true,source:'live',input:{center_hz:32256000,sample_rate:2048000,mode:'auto',gain_db:20.7,agc:false,ppm:0},display:{span_hz:600000,ref_level:-55,range_db:30,traces:{}},analysis:{},frontend_status:{epoch:1},frontend:{dc_reject_enabled:true},trigger:{armed:false},recording:{iq_active:false},vfos:[{id:1,selected:true,enabled:true,frequency_hz:32000000,bandwidth_hz:200000}],measurement:{},device:{},notifications:[]};}
function context(initial=fixture()){
  const elements=new Map(),sent=[],messages=[];
  function element(id){if(!elements.has(id))elements.set(id,{value:'',textContent:'',hidden:false,disabled:false,dataset:{},style:{},classList:{toggle(){}},setAttribute(){},focus(){this.focused=true;},getBoundingClientRect:()=>({left:100,top:50,width:1000,height:400}),setPointerCapture(){this.captured=true;},releasePointerCapture(){this.captured=false;}});return elements.get(id);}
  const c={state:initial,frame:{density:{old:true},spectrum:{f_hz:[31956000,32556000]}},frequencyPan:null,frequencyPanPending:false,connectionError:false,frontendChanging:false,drag:null,hover:null,canvasGestureTarget:'empty',dirtyForms:new Set(),frameEpoch:3,
    previousFrontend:'same',preferredSource:null,extraConditions:[],seenNotifications:new Set(),lastDeviceError:null,lastAudioConditions:'',audioSequence:0,lastAudioVfo:null,
    finite:(v,f=0)=>Number.isFinite(Number(v))?Number(v):f,clamp:(v,a,b)=>Math.max(a,Math.min(b,v)),mhz:v=>Number(v||0)/1e6,formatHz:String,
    $:element,document:{activeElement:null},asText:(id,value)=>{element(id).textContent=value;},setValue(){},updateLinks(){},inputLocked:()=>false,
    levelUnit:()=> 'dBFS',frontendRouteKey:()=> 'same',notice(){},renderCalibration(){},renderDensityRange(){},renderVfos(){},renderMeasurements(){},renderFrontend(){},updateFftVisibility(){},updatePresetLoadState(){},renderConditions(){},applyLayout(){},flushAudio(){},syncStateAudioEpoch(){},updateAudioCaption(){},audioConditionsKey:()=>'',selectedVfo:()=>c.state?.vfos?.[0],
    drawAll(){c.draws++;},draws:0,requestFrame(){c.requests++;},requests:0,cancelPendingVfoTune(){c.cancelledPending++;},cancelledPending:0,
    toast:(message,type)=>messages.push({message,type}),sent,messages,elements,
    action:async(name,data,options={})=>{if(options.guard&&!await options.guard())return null;const payload=typeof data==='function'?data():data;sent.push({name,data:clone(payload),options});return {ok:true};},console};
  vm.createContext(c);script.runInContext(c);c.plotAxesGeometry=()=>({...c.plotGeometry(1000,400),...c.frequencyViewport(c.state)});return c;
}
function gesture(c,overrides={}){return {context:c.frequencyPanContext(c.state),center:c.state.input.center_hz,span:600000,width:600,start_x:200,...overrides};}
function pointer(overrides={}){return {button:0,pointerId:7,clientX:300,clientY:435,preventDefault(){this.prevented=true;},...overrides};}

test('all three plot sizes use one receiver center/span and sample-rate ceiling',()=>{
  const c=context(),view=c.frequencyViewport(c.state);assert.equal(view.low,31956000);assert.equal(view.high,32556000);
  for(const [width,levelAxis]of [[1000,true],[420,true],[420,false]]){const g=c.plotGeometry(width,300,levelAxis);const fraction=hz=>(hz-view.low)/view.span;assert.equal(fraction(view.center),.5);assert.equal(g.left+fraction(view.center)*g.width,g.left+g.width/2);}
  c.state.display.span_hz=4000000;assert.equal(c.frequencyViewport(c.state).span,2048000);
  const preview=c.frequencyViewport(c.state,33000000);assert.equal(preview.center,33000000);assert.equal(preview.span,2048000);
});
test('heatmap pixels are anchored to their acquired frequency columns',()=>{
  const c=context(),bounds=c.heatmapFrequencyBounds({f_hz:[100,200,300],bin_hz:100},3);assert.equal(bounds.low,50);assert.equal(bounds.high,350);
  const x=(f,view,width)=>54+(f-view.low)/view.span*width;
  const old={low:0,span:400},panned={low:100,span:400};
  assert.equal(x(bounds.low,panned,800)-x(bounds.low,old,800),-200);
  assert.equal(x(200,panned,800)-x(200,old,800),-200);
  const one=c.heatmapFrequencyBounds({f_hz:[200],bin_hz:100},1);assert.equal(one.low,150);assert.equal(one.high,250);
  assert.equal(c.heatmapFrequencyBounds({f_hz:[100,100,200]},3),null);
  assert.equal(c.heatmapFrequencyBounds({f_hz:[100,200]},3),null);
  assert.equal(c.heatmapFrequencyBounds({f_hz:[100,NaN,300]},3),null);
});
test('left/right dragging moves center naturally, with fixed span and VFO Hz',()=>{
  const c=context(),before=clone(c.state.vfos),g=gesture(c);
  const left=c.frequencyPanPlan(c.state,g,100);assert.equal(left.center_hz,32356000);assert.equal(left.span_hz,600000);
  const right=c.frequencyPanPlan(c.state,g,350);assert.equal(right.center_hz,32106000);
  assert.deepEqual(c.state.vfos,before);assert.equal(c.state.display.span_hz,600000);
});
test('pan clamps hardware and fixed-mode limits without changing span',()=>{
  const c=context();c.state.input.center_hz=10000;assert.equal(c.frequencyPanPlan(c.state,gesture(c,{center:10000}),1000).center_hz,10000);
  c.state.input.center_hz=1766000000;assert.equal(c.frequencyPanPlan(c.state,gesture(c,{center:1766000000}),0).center_hz,1766000000);
  c.state.input.mode='tuner';c.state.input.center_hz=24000000;assert.equal(c.frequencyPanPlan(c.state,gesture(c,{center:24000000}),1000).center_hz,24000000);
  c.state.input.mode='direct_q';c.state.input.center_hz=28800000;assert.equal(c.frequencyPanPlan(c.state,gesture(c,{center:28800000}),0).center_hz,28800000);
  assert(c.frequencyPanPlan(c.state,gesture(c,{width:0}),200).error);assert(c.frequencyPanPlan(c.state,gesture(c),Infinity).error);
});
test('all recording locks, replay, dirty edits and disconnection prevent retune',()=>{
  const cases=[s=>{s.locks={input:true};},s=>{s.locks={center_sample_rate:true};},s=>{s.trigger.armed=true;},s=>{s.trigger.recording=true;},s=>{s.recording.iq_active=true;},s=>{s.vfos[0].wav_recording=true;},s=>{s.source='replay';}];
  for(const modify of cases){const c=context();modify(c.state);assert(c.frequencyPanPlan(c.state,gesture(c),100).error);}
  const c=context();assert(c.frequencyPanPlan(c.state,gesture(c),100,true).error);assert(c.frequencyPanPlan(c.state,gesture(c),100,false,true).error);
});
test('only the frequency-label band begins pan, without selecting/moving a VFO',()=>{
  const c=context(),before=clone(c.state.vfos);
  assert.equal(c.beginFrequencyPan(pointer({clientY:100})),false);
  const e=pointer();assert.equal(c.beginFrequencyPan(e),true);assert.equal(e.prevented,true);assert.equal(c.canvasGestureTarget,'axis');assert.equal(c.cancelledPending,1);assert.equal(c.$('spanReadout').focused,true);assert.deepEqual(c.state.vfos,before);assert.equal(c.sent.length,0);
  c.cancelFrequencyPan();assert.equal(c.frequencyPan,null);
});
test('drag preview sends no command, release sends one pre-approved center-only change',async()=>{
  const c=context(),before=clone(c.state.vfos),start=pointer();c.beginFrequencyPan(start);
  c.moveFrequencyPan(pointer({clientX:200}));assert.equal(c.sent.length,0);assert(c.frequencyPan.preview_center_hz>32256000);
  await c.finishFrequencyPan(pointer({clientX:200}));assert.equal(c.sent.length,1);assert.equal(c.sent[0].name,'set_input');assert.deepEqual(Object.keys(c.sent[0].data),['input','confirm_impacted']);assert.deepEqual(Object.keys(c.sent[0].data.input),['center_hz']);assert.equal(c.sent[0].data.confirm_impacted,true);assert.deepEqual(c.state.vfos,before);assert.equal(c.state.display.span_hz,600000);assert.equal(c.frequencyPanPending,false);
});
test('pointer cancellation and condition change discard preview without retuning',async()=>{
  const c=context();c.beginFrequencyPan(pointer());c.moveFrequencyPan(pointer({clientX:100}));c.cancelFrequencyPan();assert.equal(c.sent.length,0);
  c.beginFrequencyPan(pointer());c.state.display.span_hz=500000;await c.finishFrequencyPan(pointer({clientX:100}));assert.equal(c.sent.length,0);assert(c.messages.some(x=>x.message.includes('受信条件')));
});
test('pan confirmation exception still rechecks recording locks before a queued request',async()=>{
  const c=context(),g=gesture(c);c.action=async(name,data,options)=>{c.state.trigger.armed=true;assert.equal(await options.guard(),false);assert.equal(data.confirm_impacted,true);return null;};
  await c.commitFrequencyPan(g,32356000);assert.equal(c.sent.length,0);assert.equal(c.frequencyPanPending,false);
});
test('footer keyboard pans center only, with modifier step sizes',async()=>{
  const c=context();await c.frequencyPanKey({key:'ArrowRight',preventDefault(){}});assert.equal(c.sent[0].data.input.center_hz,32257000);
  await c.frequencyPanKey({key:'ArrowLeft',shiftKey:true,preventDefault(){}});assert.equal(c.sent[1].data.input.center_hz,32246000);
  await c.frequencyPanKey({key:'ArrowRight',altKey:true,preventDefault(){}});assert.equal(c.sent[2].data.input.center_hz,32256100);
  assert(c.sent.every(request=>request.data.confirm_impacted===true));
});
test('manual center entry still confirms impacted VFOs, while explicitly approved pan does not',async()=>{
  for(const approved of [false,true]){
    const c=context(),requests=[];let confirmations=0;
    c.window={confirm:()=>{confirmations++;return true;}};c.refreshState=async()=>{};
    c.fetch=async(_url,options)=>{const body=JSON.parse(options.body);requests.push(body);const ok=body.confirm_impacted===true;return {ok,json:async()=>ok?{ok:true}:{ok:false,impacted_vfos:[{id:1}],error:'impact'}};};
    vm.runInContext(functionSource('performAction'),c);
    const data={input:{center_hz:33000000}};if(approved)data.confirm_impacted=true;
    assert(await c.performAction('set_input',data,{}));assert.equal(confirmations,approved?0:1);assert.equal(requests.length,approved?1:2);
  }
});
test('new center VFO preserves WFM defaults and fits RF-zero and usable-band boundaries',()=>{
  const c=context(),original=clone(c.state),normal=c.centeredVfoPlan(c.state);assert.equal(normal.vfo.frequency_hz,32256000);assert.equal(normal.vfo.mode,'WFM');assert.equal(normal.vfo.bandwidth_hz,200000);assert.deepEqual(c.state,original);
  c.state.input.center_hz=10000;const low=c.centeredVfoPlan(c.state);assert.equal(low.vfo.frequency_hz,10000);assert.equal(low.vfo.mode,'WFM');assert.equal(low.vfo.bandwidth_hz,20000);assert.equal(low.vfo.frequency_hz-low.vfo.bandwidth_hz/2,0);
  c.state.input.center_hz=32256000;c.state.input.usable_bandwidth_hz=100000;assert.equal(c.centeredVfoPlan(c.state).vfo.bandwidth_hz,100000);
  c.state.input.sample_rate=1000;assert(c.centeredVfoPlan(c.state).error);
  c.state.input.sample_rate=2048000;c.state.input.center_hz=0;assert(c.centeredVfoPlan(c.state).error);assert(c.centeredVfoPlan(null).error);assert(c.centeredVfoPlan(original,true).error);
});
test('new VFO reads center after an earlier queued pan and explicitly selects only the new VFO',async()=>{
  const c=context(),old=clone(c.state.vfos[0]),requests=[],selected=[];let release,entered;
  const gate=new Promise(resolve=>{release=resolve;}),atPan=new Promise(resolve=>{entered=resolve;});
  c.actionQueue=Promise.resolve();c.actionInFlight=0;c.stateEpoch=0;c.studioClosing=false;c.studioClosed=false;
  c.performAction=async(name,data)=>{
    requests.push({name,data:clone(data)});
    if(name==='set_input'){entered();await gate;c.state.input.center_hz=data.input.center_hz;assert.equal(data.confirm_impacted,true);c.state.vfos[0].enabled=false;return {ok:true};}
    if(name==='add_vfo'){c.state.vfos.push({id:2,enabled:true,selected:false,...data.vfo});return {ok:true,state:clone(c.state)};}
    throw new Error(`Unexpected request ${name}`);
  };
  c.activateVfo=async id=>{selected.push(id);c.state.selected_vfo_id=id;c.state.vfos.forEach(v=>{v.selected=v.id===id;});return {ok:true};};
  vm.runInContext(functionSource('action'),c);
  const pan=c.commitFrequencyPan(gesture(c),33000000);await atPan;const added=c.addCenteredVfo();release();await Promise.all([pan,added]);
  assert.equal(requests.length,2);assert.equal(requests[1].name,'add_vfo');assert.equal(requests[1].data.vfo.frequency_hz,33000000);assert.deepEqual(selected,[2]);assert.equal(c.state.selected_vfo_id,2);assert.equal(c.state.vfos[0].frequency_hz,old.frequency_hz);assert.equal(c.state.vfos[0].bandwidth_hz,old.bandwidth_hz);assert.equal(c.state.vfos[0].mode,old.mode);assert.equal(c.state.vfos[0].enabled,false);assert.equal(c.state.display.span_hz,600000);
});
test('new VFO failure preserves existing VFOs and does not select a substitute',async()=>{
  const c=context(),before=clone(c.state.vfos);let selected=false;c.activateVfo=async()=>{selected=true;};
  c.connectionError=true;const result=await c.addCenteredVfo();assert.equal(result,null);assert.equal(c.sent.length,0);assert.equal(selected,false);assert.deepEqual(c.state.vfos,before);assert(c.messages.length);
});
test('span, RF gain and processing epoch changes immediately discard retained raster frames',()=>{
  for(const change of [s=>{s.display.span_hz=400000;},s=>{s.input.gain_db=49.6;},s=>{s.frontend_status.epoch=2;}]){
    const c=context(),next=clone(c.state);change(next);c.applyState(next);assert.equal(c.frame,null);assert.equal(c.frameEpoch,4);assert.equal(c.requests,1);assert.equal(c.draws,1);
  }
  const c=context();c.applyState(clone(c.state));assert.notEqual(c.frame,null);assert.equal(c.frameEpoch,3);
});
