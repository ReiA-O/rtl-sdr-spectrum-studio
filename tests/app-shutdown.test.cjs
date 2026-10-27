'use strict';
// Production lifecycle functions with fake HTTP, DOM and timers only.
// Never starts or shuts down a real server, receiver, recorder or audio device.
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const test=require('node:test'),assert=require('node:assert/strict');
const source=fs.readFileSync(path.join(__dirname,'..','web','app.js'),'utf8');
function extract(name){
  const start=new RegExp(`^  (?:async )?function ${name}\\(`,'m').exec(source);assert.ok(start,name);
  for(let end=source.indexOf('}',start.index);end>=0;end=source.indexOf('}',end+1)){
    const text=source.slice(start.index,end+1);try{new vm.Script(text);return text;}catch(error){if(!(error instanceof SyntaxError))throw error;}
  }
  throw new Error(`Unterminated ${name}`);
}
const names=['renderStudioLifecycle','shutdownConfirmation','postStudioShutdown','waitForStudioStop','shutdownStudio','action','refreshState','stateLoop','requestFrame','frameLoop','audioLoop'];
const script=new vm.Script(names.map(extract).join('\n'));
function setup(){
  const elements=new Map(),timers=new Map([[11,()=>{}],[12,()=>{}],[13,()=>{}]]),bodyClasses=new Set();let nextTimer=100;
  const make=id=>{if(!elements.has(id)){
    const classes=new Set();elements.set(id,{textContent:'',className:'',disabled:false,hidden:true,attributes:{},classList:{toggle(name,on){if(on)classes.add(name);else classes.delete(name);}},setAttribute(name,value){this.attributes[name]=value;}});
  }return elements.get(id);};
  const c={
    studioClosing:false,studioClosed:false,stateTimer:11,frameTimer:12,audioTimer:13,
    actionQueue:Promise.resolve(),actionInFlight:0,stateEpoch:0,frameEpoch:5,framePending:false,frameRefreshRequested:false,frameFailures:0,
    state:{running:true,source:'live',recording:{iq_active:false},trigger:{armed:false},vfos:[{id:1,wav_recording:false}],device:{}},
    frame:null,connectionError:false,audioEnabled:true,audioStarting:false,audioEpoch:3,audioSequence:0,
    cancelledTunes:0,flushes:0,audioRenders:0,captionUpdates:0,appliedStates:0,acceptedFrames:0,playedPackets:0,performedActions:0,
    snapshots:0,prompts:[],requests:[],errors:[],notices:[],cleared:[],timers,bodyClasses,elements,
    AbortController,Number,Promise,document:{hidden:false,body:{classList:{toggle(name,on){if(on)bodyClasses.add(name);else bodyClasses.delete(name);}}}},
    $:make,asText:(id,value)=>{make(id).textContent=value;},
    cancelPendingVfoTune:()=>{c.cancelledTunes++;},flushAudio:()=>{c.flushes++;c.audioEpoch++;},
    renderAudioControl:()=>{c.audioRenders++;},updateAudioCaption:()=>{c.captionUpdates++;},
    updatePresetLoadState(){},renderVfos(){},renderFrontend(){},notice:(...v)=>c.notices.push(v),toast:(...v)=>c.errors.push(v),
    applyState:()=>{c.appliedStates++;},acceptFrame:()=>{c.acceptedFrames++;},
    finite:(n,f=0)=>Number.isFinite(Number(n))?Number(n):f,audioPacketMatchesState:()=>true,playPcm:()=>{c.playedPackets++;},
    window:{confirm:message=>{c.prompts.push(message);return true;}},
    getJson:async path=>{assert.equal(path,'/api/state');c.snapshots++;return c.state;},
    performAction:async()=>{c.performedActions++;return {ok:true};},
    setTimeout(fn,ms){const id=nextTimer++;timers.set(id,{fn,ms});if(ms===150)queueMicrotask(fn);return id;},
    clearTimeout(id){timers.delete(id);c.cleared.push(id);},
    fetch:async(url,options={})=>{
      c.requests.push({url,method:options.method||'GET',body:options.body});
      if(url==='/api/action')return {ok:true,status:200,json:async()=>({ok:true,shutdown:true})};
      if(url==='/api/health')throw new TypeError('connection refused');
      throw new Error('Unexpected request '+url);
    },
  };
  c.document.querySelectorAll=selector=>selector.startsWith('canvas')?[make('spectrumCanvas'),make('spectrumResizer')]:['shutdownButton','receiveButton','audioButton','centerMHz'].map(make);
  vm.createContext(c);script.runInContext(c);return c;
}

test('normal shutdown requires explicit ACK and server disappearance before showing closed',async()=>{
  const c=setup();const result=await c.shutdownStudio();assert.equal(result,true);
  assert.equal(c.studioClosed,true);assert.equal(c.studioClosing,false);
  assert.equal(c.requests.filter(x=>x.url==='/api/action').length,1);
  assert.deepEqual(JSON.parse(c.requests[0].body),{action:'shutdown'});
  assert.equal(c.requests[1].url,'/api/health');
  assert.equal(c.$('sessionStatus').textContent,'終了しました');
  assert.match(c.$('shutdownNotice').textContent,/終了しました.*タブを閉じて/);
  assert.equal(c.$('shutdownButton').disabled,true);assert.equal(c.$('receiveButton').disabled,true);
  assert.equal(c.audioEnabled,false);assert.equal(c.flushes,1);assert.equal(c.audioRenders,1);
  assert.ok(c.bodyClasses.has('studio-closed'));assert.ok(!c.bodyClasses.has('studio-closing'));
  assert.ok([11,12,13].every(id=>c.cleared.includes(id)));
});

test('cancel leaves the server and enabled audio untouched',async()=>{
  const c=setup();c.window.confirm=message=>{c.prompts.push(message);return false;};
  assert.equal(await c.shutdownStudio(),false);assert.equal(c.studioClosed,false);assert.equal(c.studioClosing,false);
  assert.equal(c.requests.length,0);assert.equal(c.audioEnabled,true);assert.equal(c.flushes,0);
  assert.equal(c.$('shutdownButton').disabled,false);assert.equal(c.$('shutdownNotice').hidden,true);
  assert.match(c.prompts[0],/未適用の入力・設定は適用されません/);
});

test('recording and trigger confirmation explicitly says recordings will be finalized',()=>{
  const c=setup();
  for(const snapshot of [{recording:{iq_active:true}},{trigger:{armed:true}},{trigger:{recording:true}},{vfos:[{wav_recording:true}]}])
    assert.match(c.shutdownConfirmation(snapshot),/保存を確定して終了/);
  assert.doesNotMatch(c.shutdownConfirmation({}),/保存を確定/);
});

test('queued action finishes before fresh recording state is confirmed; repeated click is ignored',async()=>{
  const c=setup();let release;c.actionQueue=new Promise(resolve=>{release=()=>{c.state.recording.iq_active=true;resolve();};});
  const pending=c.shutdownStudio();assert.equal(c.studioClosing,true);assert.equal(c.$('shutdownButton').disabled,true);
  assert.equal(c.snapshots,0);assert.equal(c.prompts.length,0);assert.equal(await c.shutdownStudio(),false);
  assert.equal(await c.action('set_input',{input:{gain_db:30}}),null);assert.equal(c.performedActions,0);
  release();assert.equal(await pending,true);assert.equal(c.snapshots,1);assert.equal(c.prompts.length,1);
  assert.match(c.prompts[0],/保存を確定して終了/);assert.equal(c.requests.filter(x=>x.url==='/api/action').length,1);
});

test('HTTP rejection never claims closed and permits retry',async()=>{
  const c=setup(),normal=c.fetch;
  c.fetch=async(url,options)=>url==='/api/action'?{ok:false,status:500,json:async()=>({error:'flush failed'})}:normal(url,options);
  assert.equal(await c.shutdownStudio(),false);assert.equal(c.studioClosed,false);assert.equal(c.studioClosing,false);
  assert.equal(c.$('shutdownButton').disabled,false);assert.match(c.$('shutdownNotice').textContent,/終了を確認できません.*flush failed/);
  assert.notEqual(c.$('sessionStatus').textContent,'終了しました');assert.equal(c.audioEnabled,true);
  c.fetch=normal;assert.equal(await c.shutdownStudio(),true);
});

test('missing shutdown acknowledgment and unavailable initial state do not show success',async()=>{
  const c=setup();c.fetch=async()=>({ok:true,status:200,json:async()=>({ok:true})});
  assert.equal(await c.shutdownStudio(),false);assert.equal(c.studioClosed,false);assert.equal(c.$('shutdownButton').disabled,false);
  const offline=setup();offline.getJson=async()=>{throw new Error('offline');};
  assert.equal(await offline.shutdownStudio(),false);assert.equal(offline.requests.length,0);assert.equal(offline.prompts.length,0);
  assert.equal(offline.$('shutdownButton').disabled,false);
});

test('an ACK while the server still responds or health times out is not a completed exit',async()=>{
  const c=setup(),normal=c.fetch;c.fetch=async(url,options)=>url==='/api/health'?{ok:true}:normal(url,options);
  assert.equal(await c.shutdownStudio(),false);assert.equal(c.studioClosed,false);assert.equal(c.$('shutdownButton').disabled,false);
  assert.match(c.$('shutdownNotice').textContent,/終了要求は送信しましたが/);
  const timed=setup(),normalTimed=timed.fetch;timed.fetch=async(url,options)=>{
    if(url!=='/api/health')return normalTimed(url,options);const error=new Error('timed out');error.name='AbortError';throw error;
  };
  assert.equal(await timed.shutdownStudio(),false);assert.equal(timed.studioClosed,false);
});

test('closed lifecycle stops all polling and refuses future actions',async()=>{
  const c=setup();await c.shutdownStudio();c.requests.length=0;c.snapshots=0;c.timers.clear();
  await c.stateLoop();await c.frameLoop();await c.audioLoop();await c.refreshState();c.requestFrame();
  assert.equal(await c.action('start'),null);assert.equal(c.performedActions,0);
  assert.equal(c.snapshots,0);assert.equal(c.requests.length,0);assert.equal(c.timers.size,0);
});

test('late state, frame and audio responses cannot repaint or reschedule after exit',async()=>{
  for(const method of ['refreshState','frameLoop','audioLoop']){
    const c=setup();let release;c.getJson=()=>new Promise(resolve=>{release=resolve;});
    const pending=c[method]();c.studioClosed=true;c.audioEnabled=false;c.timers.clear();
    release({data_b64:'AAAA',sequence:2});await pending;
    assert.equal(c.appliedStates,0,method);assert.equal(c.acceptedFrames,0,method);assert.equal(c.playedPackets,0,method);assert.equal(c.timers.size,0,method);
  }
});
