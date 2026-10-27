'use strict';
// Pure UI tests: evaluate the real app functions with fake DOM/actions. No HTTP,
// browser, receiver, audio device, or native process is opened.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const assert = require('node:assert/strict');
const source = fs.readFileSync(path.join(__dirname, '..', 'web', 'app.js'), 'utf8');

function functionSource(name) {
  const match = new RegExp(`^  (?:async )?function ${name}\\(`, 'm').exec(source);
  assert.ok(match, `Function ${name} exists in app.js`);
  const opening = source.indexOf('{', source.indexOf(')', match.index));
  let depth = 0;
  for (let i = opening; i < source.length; i++) {
    if (source[i] === '{') depth++;
    if (source[i] === '}' && --depth === 0) return source.slice(match.index, i + 1);
    // Closing braces alone decrement depth; all tested strings/templates have
    // balanced braces. The returned function is independently parsed by V8.
  }
  throw new Error(`Unterminated app function ${name}`);
}
const names = ['frontendPresentation','frontendRouteKey','frontendFrameMatchesState','frontendDisplayRoute',
  'renderFrontend','changeFrontend','peakTunePlan','tunePeakFrequency','acceptFrame','refreshState','applyState'];
const script = new vm.Script(names.map(functionSource).join('\n'));
const clone = value => JSON.parse(JSON.stringify(value));

function state(enabled = true, extra = {}) {
  return {running:true,source:'live',frontend:{dc_reject_enabled:enabled},
    frontend_status:{enabled,applied:enabled,available:true,reason:enabled?'enabled':'off',cutoff_hz:20,input_mode:'tuner'},
    input:{center_hz:32256000,sample_rate:2048000,gain_db:20.7,agc:false,ppm:0,mode:'auto'},
    analysis:{},display:{},measurement:{},trigger:{armed:false},recording:{iq_active:false},device:{},
    vfos:[{id:7,selected:true,enabled:true,frequency_hz:32000000,bandwidth_hz:200000,mode:'WFM',wav_recording:false}],
    notifications:[],...extra};
}
function context(initial = state()) {
  const elements = new Map(), edits = new Map(), sent = [], messages = [];
  const element = id => {
    if (!elements.has(id)) elements.set(id,{value:'',checked:false,disabled:false,hidden:false,textContent:'',className:'',
      setAttribute(key,value){this[key]=value;},classList:{toggle(){}},querySelectorAll(){return [];}});
    return elements.get(id);
  };
  const c = {state:initial,frame:{spectrum:{f_hz:[1],unit:'dBFS'}},connectionError:false,
    frontendChanging:false,frontendPendingValue:null,frameEpoch:3,stateEpoch:10,actionInFlight:0,
    preferredSource:null,dirtyForms:new Set(),extraConditions:[],seenNotifications:new Set(),drag:null,
    lastDeviceError:null,lastAudioConditions:'',audioSequence:0,lastAudioVfo:null,
    document:{activeElement:null},$ : element,
    finite:(n,f=0)=>Number.isFinite(Number(n))?Number(n):f,
    mhz:n=>Number(n||0)/1e6,formatHz:n=>String(n),
    levelUnit:()=> 'dBFS',asText:(id,value)=>{element(id).textContent=value;},
    setValue(){},updateLinks(){},inputLocked:()=>false,notice(){},renderCalibration(){},renderDensityRange(){},renderVfos(){},renderMeasurements(){},
    renderDensityHistory(){},updateFftVisibility(){},updatePresetLoadState(){},applyLayout(){},
    drawAll(){},flushAudio(){},syncStateAudioEpoch(){},updateAudioCaption(){},audioConditionsKey:()=>'',
    requestFrame(){c.frameRequests++;},frameRequests:0,
    cancelPendingVfoTune(){c.tuneCancellations++;},tuneCancellations:0,
    focusSpectrum(){c.focused=true;},toast:(message,type)=>messages.push({message,type}),
    selectedVfo:()=>c.state?.vfos?.find(v=>v.selected)||null,
    vfoEditState:id=>{if(!edits.has(id))edits.set(id,{basic:{dirty:false},reception:{dirty:false}});return edits.get(id);},
    activateVfo:()=>Promise.resolve(true),
    action:async(name,data,options={})=>{if(options.guard&&!await options.guard())return null;sent.push({name,data:clone(data)});return {ok:true};},
    elements,edits,sent,messages,console};
  vm.createContext(c);script.runInContext(c);return c;
}

test('unknown or disconnected frontend never claims correction is active', () => {
  const c=context();
  assert.equal(c.frontendPresentation(null).known,false);
  assert.equal(c.frontendPresentation({running:true}).applied,false);
  assert.equal(c.frontendPresentation(state(),true).applied,false);
  assert.match(c.frontendPresentation(state(),true).label,/接続待ち/);
  assert.equal(c.frontendPresentation(state()).applied,true);
});
test('backend bypass and unsupported sample rate are shown as inapplicable', () => {
  const c=context();
  const direct=state(true,{frontend_status:{enabled:true,available:false,applied:false,reason:'direct_sampling',input_mode:'direct_q'}});
  assert.match(c.frontendPresentation(direct).label,/適用外.*Direct/);
  const rate=state(true,{frontend_status:{enabled:true,available:false,applied:false,reason:'sample_rate_unavailable',input_mode:'tuner'}});
  assert.match(c.frontendPresentation(rate).label,/サンプルレート/);
});
test('stopped setting is separate from the processing route of retained plots', () => {
  const stopped=state(true,{running:false,frontend_status:{enabled:true,applied:false,available:true,reason:'stopped',input_mode:'tuner'}}),c=context(stopped);
  assert.match(c.frontendPresentation(stopped).label,/ON設定.*停止中/);
  assert.match(c.frontendDisplayRoute(stopped,{spectrum:{f_hz:[1,2]}}),/停止前の解析結果/);
  assert.equal(c.frontendDisplayRoute(stopped,null),'受信停止中');
  c.renderFrontend();assert.match(c.$('footerRight').textContent,/停止前の解析結果/);
  assert.doesNotMatch(c.$('footerRight').textContent,/補正は未適用/);
});
test('all recording and arm paths lock correction changes', () => {
  const c=context();
  const variants=[state(true,{trigger:{armed:true}}),state(true,{trigger:{recording:true}}),
    state(true,{recording:{iq_active:true}}),state(true,{locks:{frontend:true,frontend_reason:'保存中'}}),
    state(true,{vfos:[{wav_recording:true}]})];
  for(const current of variants)assert.equal(c.frontendPresentation(current).disabled,true);
  assert.equal(c.frontendPresentation(state()).disabled,false);
});
test('checkbox reflects backend state and rejects a click while recording', async () => {
  const c=context(state(true,{recording:{iq_active:true}}));
  c.$('dcRejectEnabled').checked=false;await c.changeFrontend();
  assert.equal(c.sent.length,0);assert.equal(c.$('dcRejectEnabled').checked,true);assert.equal(c.$('dcRejectEnabled').disabled,true);
});
test('frontend change sends only explicit correction setting and clears pending UI state', async () => {
  const c=context();c.$('dcRejectEnabled').checked=false;await c.changeFrontend();
  assert.deepEqual(c.sent,[{name:'set_frontend',data:{frontend:{dc_reject_enabled:false}}}]);
  assert.equal(c.frontendChanging,false);assert.equal(c.frontendPendingValue,null);
});
test('new backend does not accept frames with old correction or mode status', () => {
  const c=context(),frame={frontend_status:clone(c.state.frontend_status),spectrum:{f_hz:[10],unit:'dBFS'}};
  assert.equal(c.frontendFrameMatchesState(c.state,frame),true);
  assert.equal(c.frontendFrameMatchesState(c.state,{spectrum:{}}),false);
  assert.equal(c.frontendFrameMatchesState(c.state,{...frame,frontend_status:{...frame.frontend_status,enabled:false}}),false);
  assert.equal(c.frontendFrameMatchesState(c.state,{...frame,frontend_status:{...frame.frontend_status,input_mode:'direct_i'}}),false);
  const previous=c.frame;c.acceptFrame({...frame,frontend_status:{...frame.frontend_status,enabled:false}});assert.equal(c.frame,previous);
  c.acceptFrame(frame);assert.equal(c.frame,frame);
});
test('correction changes clear retained display before rendering new state', () => {
  const c=context();const next=state(false);c.applyState(next);
  assert.equal(c.state,next);assert.equal(c.frame,null);assert.equal(c.frameEpoch,4);assert.ok(c.frameRequests>0);
});
test('same correction setting preserves frame; server audio epoch reset is accepted', () => {
  const c=context();c.state.audio_epoch=99;const next=state(true);next.audio_epoch=0;const prior=c.frame;
  c.applyState(next);assert.equal(c.state.audio_epoch,0);assert.equal(c.frame,prior);assert.equal(c.frameEpoch,3);
});
test('frame correction epoch must match, including a new server restarting at zero', () => {
  const c=context();c.state.frontend_status.epoch=0;
  const fresh={frontend_status:{...c.state.frontend_status,epoch:0}},old={frontend_status:{...c.state.frontend_status,epoch:99}};
  assert.equal(c.frontendFrameMatchesState(c.state,old),false);
  assert.equal(c.frontendFrameMatchesState(c.state,fresh),true);
  assert.equal(c.frontendFrameMatchesState(c.state,{frontend_status:{...fresh.frontend_status,epoch:-1}}),false);
});
test('pending state poll cannot overwrite a newer OFF action state', async () => {
  const c=context();let resolve;c.getJson=()=>new Promise(r=>{resolve=r;});
  let applied=0;c.applyState=next=>{applied++;c.state=next;};
  const pending=c.refreshState();c.stateEpoch++;c.state=state(false);resolve(state(true));await pending;
  assert.equal(applied,0);assert.equal(c.state.frontend.dc_reject_enabled,false);
});
test('selected VFO is tuned with no center, mode or bandwidth mutation', async () => {
  const c=context();await c.tunePeakFrequency(32106000,'A1');
  assert.deepEqual(c.sent,[{name:'update_vfo',data:{id:7,patch:{frequency_hz:32106000}}}]);assert.equal(c.focused,true);
});
test('out-of-band peak checks the complete VFO filter width', async () => {
  const c=context();const edge=32256000+2048000*.9/2;
  assert.ok(c.peakTunePlan(c.state,c.selectedVfo(),edge).error);
  assert.equal(c.peakTunePlan(c.state,c.selectedVfo(),edge-100000).id,7);
  await c.tunePeakFrequency(edge,'A1');assert.equal(c.sent.length,0);
});
test('dirty VFO settings are not discarded by peak selection', async () => {
  const c=context();c.vfoEditState(7).basic.dirty=true;await c.tunePeakFrequency(32106000);
  assert.equal(c.sent.length,0);assert.equal(c.tuneCancellations,0);
});
test('failed VFO selection prevents subsequent tune request', async () => {
  const c=context();c.activateVfo=()=>Promise.resolve(null);await c.tunePeakFrequency(32106000);
  assert.equal(c.sent.length,0);
});
test('queued tune rechecks target existence and current input band', async () => {
  const c=context();c.activateVfo=async()=>{c.state.input.center_hz=100000000;return true;};
  await c.tunePeakFrequency(32106000);assert.equal(c.sent.length,0);
  const deleted=context();deleted.activateVfo=async()=>{deleted.state.vfos=[];return true;};
  await deleted.tunePeakFrequency(32106000);assert.equal(deleted.sent.length,0);
});
test('peak selection is guarded during correction switch, disconnection and stopped VFO', async () => {
  const c=context();c.frontendChanging=true;await c.tunePeakFrequency(32106000);assert.equal(c.sent.length,0);
  c.frontendChanging=false;c.connectionError=true;await c.tunePeakFrequency(32106000);assert.equal(c.sent.length,0);
  c.connectionError=false;c.state.vfos[0].enabled=false;await c.tunePeakFrequency(32106000);assert.equal(c.sent.length,0);
});
