'use strict';
// Fresh SSB UI contract tests. They execute real UI functions with DOM/action
// doubles only; no receiver, network, browser, speaker or saved session is used.
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const test=require('node:test'),assert=require('node:assert/strict');
const source=fs.readFileSync(path.join(__dirname,'../web/app.js'),'utf8');
function functionSource(name){
  const match=new RegExp(`^  (?:async )?function ${name}\\(`,'m').exec(source);assert(match,`${name} exists`);
  for(let end=source.indexOf('}',match.index);end>=0;end=source.indexOf('}',end+1)){
    const candidate=source.slice(match.index,end+1);try{new vm.Script(`(${candidate})`);return candidate;}catch(error){if(!(error instanceof SyntaxError))throw error;}
  }
  throw new Error(`Unterminated ${name}`);
}
const names=['vfoPassband','boundedVfoBandwidth','vfoBandwidthFromEdge','peakTunePlan','peakTuneButton','tunePeakFrequency',
  'vfoEditState','markVfoEdited','receptionConfig','wavRouteLabel','readVfoBasic','updateVfoModeUi','changeVfoMode','receptionReadout',
  'vfoCardHtml','drawSpectrum','beginVfoDrag','moveVfo','finishVfo','vfoKey','frequencyPanContext','frequencyPanPolicy','frequencyPanPlan'];
const script=new vm.Script(names.map(functionSource).join('\n'));
const clone=value=>JSON.parse(JSON.stringify(value)),flush=()=>new Promise(resolve=>setImmediate(resolve));
function fixture(mode='USB'){
  return {running:true,source:'demo',input:{center_hz:32000000,sample_rate:2048000,mode:'auto'},display:{span_hz:10000,traces:{}},
    vfos:[{id:7,selected:true,enabled:true,frequency_hz:32000000,bandwidth_hz:2700,mode}],trigger:{armed:false},recording:{iq_active:false},measurement:{}};
}
function context(initial=fixture()){
  const elements=new Map(),sent=[],messages=[],rectangles=[],texts=[];
  const element=id=>{if(!elements.has(id))elements.set(id,{dataset:{},value:'',hidden:false,setAttribute(){},classList:{toggle(){}},setPointerCapture(){},releasePointerCapture(){}});return elements.get(id);};
  const drawing={save(){},beginPath(){},rect(){},clip(){},stroke(){},moveTo(){},lineTo(){},setLineDash(){},closePath(){},fill(){},restore(){},
    fillRect(...args){rectangles.push(args);},fillText(...args){texts.push(args);}};
  const c={state:initial,frame:{spectrum:{}},connectionError:false,frontendChanging:false,drag:null,hover:null,dirtyForms:new Set(),vfoEdits:new Map(),
    frequencyPan:null,frequencyPanPending:false,canvasGestureTarget:'empty',vfoSelectionPromise:Promise.resolve(true),
    $:element,asText:(id,text)=>{element(id).textContent=text;},finite:(n,f=0)=>Number.isFinite(Number(n))?Number(n):f,
    clamp:(n,lo,hi)=>Math.max(lo,Math.min(hi,n)),mhz:n=>Number(n||0)/1e6,escapeHtml:String,dbText:String,validLevel:Number.isFinite,
    colors:['#abcdef'],receptionDefaults:{afc_enabled:false,afc_max_hz:1000,squelch_enabled:false,wav_audio:'demod'},
    vfoReceptionHtml:()=>'',selectedVfo:()=>c.state?.vfos?.[0],selectedUserMarker:()=>null,userMarkers:()=>[],measurementMarkers:()=>[],
    toast:(message,type)=>messages.push({message,type}),focusSpectrum(){},cancelPendingVfoTune(){},activateVfo:async()=>true,
    userMarkerSelected:()=>false,wireId:id=>Number(id),sent,messages,elements,rectangles,texts,
    action:async(name,data,options={})=>{if(options.guard&&!await options.guard())return null;const body=typeof data==='function'?data():data;sent.push({name,data:clone(body)});return {ok:true};},
    plotAxes:()=>({ctx:drawing,p:{left:0,top:0,width:1000,height:200},low:31995000,high:32005000,span:10000,x:f=>(f-31995000)/10,y:db=>100-db}),
    plotFrequency:event=>({x:(event.hz-31995000)/10,y:50,hz:event.hz,s:{left:0,top:0,width:1000,height:200,low:31995000,span:10000}}),
    console};
  vm.createContext(c);script.runInContext(c);return c;
}
function card(mode='AM'){
  const fields={frequency:{value:'32'},bandwidth:{value:'10'},mode:{value:mode,dataset:{vfoField:'mode'}},'wav-format':{value:'PCM16'},afc_enabled:{checked:true},afc_max_hz:{value:'1000'},
    'frequency-label':{},'mode-note':{},'afc-note':{},status:{}};
  const el={dataset:{vfo:'7'},fields,querySelector(selector){
    const match=/\[data-(?:vfo-field|reception-field)=([^\]]+)\]/.exec(selector);if(match)return fields[match[1]]||null;
    if(selector==='[data-vfo-frequency-label]')return fields['frequency-label'];
    if(selector==='[data-vfo-mode-note]')return fields['mode-note'];if(selector==='[data-vfo-afc-note]')return fields['afc-note'];
    if(selector.startsWith('[data-vfo-edit-status='))return fields.status;return null;
  }};fields.mode.closest=()=>el;return el;
}
function pointer(hz){return {hz,button:0,pointerId:1,preventDefault(){},shiftKey:false};}

test('SSB RF geometry uses a suppressed carrier and one side, retaining AM/FM geometry',()=>{
  const c=context();
  for(const [mode,low,high]of [['USB',32000300,32003000],['LSB',31997000,31999700],['AM',31998650,32001350],['NFM',31998650,32001350],['WFM',31998650,32001350]]){
    const band=c.vfoPassband(32000000,2700,mode);assert.equal(band.low_hz,low);assert.equal(band.high_hz,high);
  }
  for(const bw of [999,14101,NaN])assert.equal(c.vfoPassband(32000000,bw,'USB'),null);
  for(const bw of [1000,14100])assert(c.vfoPassband(32000000,bw,'LSB'));
  assert.equal(c.vfoPassband(-1,2700,'USB'),null);assert.equal(c.vfoPassband(32000000,2700,'CW'),null);
});
test('peak placement uses the center of the SSB passband without claiming carrier recovery',()=>{
  const c=context(),before=clone(c.state);
  for(const [mode,reference]of [['USB',31998350],['LSB',32001650],['AM',32000000]]){
    const v={...c.state.vfos[0],mode},plan=c.peakTunePlan(c.state,v,32000000);assert.equal(plan.patch.frequency_hz,reference);
    const band=c.vfoPassband(reference,2700,mode);assert.equal((band.low_hz+band.high_hz)/2,32000000);
  }
  const v={...c.state.vfos[0],bandwidth_hz:4000};assert.equal(c.peakTunePlan(c.state,v,32000000).patch.frequency_hz,31997700);
  assert.deepEqual(c.state,before);assert.match(c.peakTuneButton({frequency_hz:32000000},'A1'),/帯域内に配置・基準の微調整が必要/);
});
test('peak placement checks both reference and full asymmetric RF band at one-hertz boundaries',()=>{
  const c=context();c.state.input.usable_bandwidth_hz=10000;const v=c.state.vfos[0];
  for(const [mode,peak,outside]of [['USB',32003650,1],['USB',31996650,-1],['LSB',31996350,-1],['LSB',32003350,1]]){
    v.mode=mode;assert(c.peakTunePlan(c.state,v,peak).patch);assert(c.peakTunePlan(c.state,v,peak+outside).error);
  }
  c.state.input.center_hz=10000;c.state.input.usable_bandwidth_hz=20000;v.mode='LSB';
  assert(c.peakTunePlan(c.state,v,1350).patch);assert(c.peakTunePlan(c.state,v,1349).error);
  v.mode='USB';assert(c.peakTunePlan(c.state,v,1650).error);assert(c.peakTunePlan(c.state,v,1651).patch);
});
test('SSB basic apply atomically disables AFC and validates width and RF zero',()=>{
  const c=context(),el=card('USB');el.fields.bandwidth.value='2.7';const read=c.readVfoBasic(el);
  assert.equal(read.patch.mode,'USB');assert.equal(read.patch.bandwidth_hz,2700);assert.equal(read.patch.reception.afc_enabled,false);
  for(const width of ['0.999','14.101','NaN']){el.fields.bandwidth.value=width;assert(c.readVfoBasic(el).error);}
  el.fields.bandwidth.value='2.7';el.fields.mode.value='LSB';el.fields.frequency.value='0.002999';assert(c.readVfoBasic(el).error);
  el.fields.frequency.value='0.003';assert(c.readVfoBasic(el).patch);
  el.fields.mode.value='AM';el.fields.frequency.value='32';assert.equal(c.readVfoBasic(el).patch.reception,undefined);
});
test('explicit mode selection sets 2.7 kHz and unavailable AFC with 10-Hz numeric tuning',()=>{
  const c=context(),el=card('USB'),before=clone(c.state);
  c.changeVfoMode({target:el.fields.mode});assert.equal(el.fields.bandwidth.value,2.7);assert.equal(el.fields.bandwidth.min,'1');assert.equal(el.fields.bandwidth.max,'14.1');
  assert.equal(el.fields.frequency.step,'0.00001');assert.equal(el.fields.afc_enabled.checked,false);assert.equal(el.fields.afc_enabled.disabled,true);
  assert.equal(el.fields.afc_max_hz.disabled,true);assert.match(el.fields['mode-note'].textContent,/300 Hz/);assert.match(el.fields['frequency-label'].textContent,/抑圧搬送波/);
  assert.deepEqual(c.state,before);assert.equal(c.sent.length,0);assert.equal(c.vfoEditState(7).basic.dirty,true);
  for(const [mode,width]of [['AM',10],['NFM',12.5],['WFM',200]]){el.fields.mode.value=mode;c.changeVfoMode({target:el.fields.mode});assert.equal(el.fields.bandwidth.value,width);assert.equal(el.fields.afc_enabled.disabled,false);assert.equal(el.fields.frequency.step,'0.001');}
});
test('SSB readout never reports AFC waiting or LOCK, even with an inconsistent old readout',()=>{
  const c=context(),v=c.state.vfos[0];v.reception={afc_enabled:true};v.reception_status={available:true,afc_status:'tracking',afc_locked:true,afc_offset_hz:100};
  for(const running of [false,true]){const read=c.receptionReadout(v,running);assert.match(read.afc,/適用外.*手動同調/);assert.doesNotMatch(read.afc,/LOCK|追従中|待機/);}
});
test('spectrum shade and bandwidth handle are one-sided, while carrier-reference line stays fixed',()=>{
  const c=context();
  for(const [mode,x]of [['USB',530],['LSB',200]]){
    c.state.vfos[0].mode=mode;c.rectangles.length=0;c.texts.length=0;c.drawSpectrum();
    assert.deepEqual(c.rectangles[0],[x,0,270,200]);assert.equal(c.rectangles.filter(rect=>rect[2]===4&&rect[3]===11).length,1);
    assert(c.texts.some(text=>text[0]===`V7 ${mode}`&&text[1]===500));
  }
  c.state.vfos[0].mode='AM';c.rectangles.length=0;c.drawSpectrum();assert.equal(c.rectangles.filter(rect=>rect[2]===4&&rect[3]===11).length,2);
});
test('only the outer SSB edge resizes, and resizing preserves the carrier reference',async()=>{
  const c=context(),v=c.state.vfos[0];
  c.beginVfoDrag(pointer(32000300));assert.equal(c.drag,null,'fixed inner USB edge is not a width handle');
  c.beginVfoDrag(pointer(32003000));assert.equal(c.drag.kind,'high');c.moveVfo(pointer(32004300));assert.equal(c.drag.bandwidth_hz,4000);assert.equal(c.drag.frequency_hz,32000000);
  await c.finishVfo(pointer(32004300));assert.equal(c.sent[0].data.patch.frequency_hz,32000000);assert.equal(c.sent[0].data.patch.bandwidth_hz,4000);
  v.mode='LSB';c.beginVfoDrag(pointer(31999700));assert.equal(c.drag,null,'fixed inner LSB edge is not a width handle');
  c.beginVfoDrag(pointer(31997000));assert.equal(c.drag.kind,'low');c.moveVfo(pointer(31994700));assert.equal(c.drag.bandwidth_hz,5000);assert.equal(c.drag.frequency_hz,32000000);
});
test('SSB outer-edge width clamps engineering limits and AM edge resizing stays symmetric',()=>{
  const c=context();assert.equal(c.vfoBandwidthFromEdge('USB',32000000,32000400),1000);assert.equal(c.vfoBandwidthFromEdge('USB',32000000,32100000),14100);
  assert.equal(c.vfoBandwidthFromEdge('LSB',32000000,31999600),1000);assert.equal(c.vfoBandwidthFromEdge('LSB',32000000,31900000),14100);
  assert.equal(c.vfoBandwidthFromEdge('AM',32000000,32003000),6000);
});
test('queued keyboard width edits use latest SSB bandwidth and retain absolute Hz',async()=>{
  const c=context(),v=c.state.vfos[0];c.vfoKey({key:'+',preventDefault(){}});v.bandwidth_hz=14000;await flush();
  assert.equal(c.sent[0].data.patch.bandwidth_hz,14100);assert.equal(c.sent[0].data.patch.frequency_hz,undefined);assert.equal(v.frequency_hz,32000000);
  c.vfoKey({key:'-',shiftKey:true,preventDefault(){}});await flush();assert.equal(c.sent[1].data.patch.bandwidth_hz,4000);
});
test('SSB peak action sends only the adjusted reference, preserving mode and bandwidth',async()=>{
  const c=context(),before=clone(c.state.vfos);await c.tunePeakFrequency(32000000,'A1');
  assert.equal(c.sent.length,1);assert.deepEqual(c.sent[0],{name:'update_vfo',data:{id:7,patch:{frequency_hz:31998350}}});
  assert.deepEqual(c.state.vfos,before);assert.match(c.messages[0].message,/基準の微調整が必要/);
});
test('SSB mode menu includes both sidebands, with existing modes and fresh center pan non-follow retained',()=>{
  const c=context(),html=c.vfoCardHtml(c.state.vfos[0],0);for(const mode of ['AM','NFM','WFM','USB','LSB'])assert(html.includes(`<option>${mode}</option>`));
  const before=clone(c.state.vfos),plan=c.frequencyPanPlan(c.state,{context:c.frequencyPanContext(c.state),center:32000000,span:10000,width:1000,start_x:500},400);
  assert.equal(plan.center_hz,32001000);assert.deepEqual(c.state.vfos,before);
});
