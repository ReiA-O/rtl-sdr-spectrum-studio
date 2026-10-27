'use strict';
// Exercise the production audio toggle with a fake DOM and WebAudio context.
// No browser, speaker, HTTP server, receiver or instrument is opened.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const assert = require('node:assert/strict');
const source = fs.readFileSync(path.join(__dirname, '..', 'web', 'app.js'), 'utf8');

function extract(name) {
  const start = new RegExp(`^  (?:async )?function ${name}\\(`, 'm').exec(source);
  assert.ok(start, `Production ${name} function exists`);
  for (let end = source.indexOf('}', start.index); end >= 0; end = source.indexOf('}', end + 1)) {
    const candidate = source.slice(start.index, end + 1);
    try { new vm.Script(candidate); return candidate; }
    catch (error) { if (!(error instanceof SyntaxError)) throw error; }
  }
  throw new Error(`Unterminated ${name}`);
}
const script = new vm.Script(['renderAudioControl', 'updateAudioCaption', 'toggleAudio'].map(extract).join('\n'));

function setup(options = {}) {
  const elements = new Map();
  const c = {
    audioEnabled:false, audioStarting:false, audioContext:null, audioGain:null, studioClosing:false, studioClosed:false,
    audioSequence:19, audioReceivedAt:0, connectionError:false,
    state:{running:true}, performance:{now:()=>5000},
    selectedVfo:()=>({id:2,mode:'WFM',frequency_hz:32000000}),
    mhz:n=>n/1e6, num:()=>.2, flushes:0, constructions:0, resumes:0, errors:[],
    $(id) {
      if (!elements.has(id)) {
        const classes = new Set();
        elements.set(id,{textContent:'',disabled:false,title:'',attributes:{},classes,
          classList:{toggle(name,on){if(on)classes.add(name);else classes.delete(name);}},
          setAttribute(name,value){this.attributes[name]=value;}});
      }
      return elements.get(id);
    },
    asText(id,value){c.$(id).textContent=value;},
    flushAudio(){c.flushes++;}, toast(message,type){c.errors.push({message,type});},
    window:{AudioContext:class {
      constructor(){c.constructions++;if(options.constructorError)throw new Error('constructor failed');this.state='suspended';this.destination={};}
      createGain(){return {gain:{value:0},connect(){}};}
      async resume(){c.resumes++;if(options.resume)await options.resume(this);else this.state='running';}
    }}, elements,
  };
  vm.createContext(c);script.runInContext(c);return c;
}

function assertOff(c) {
  const b=c.$('audioButton');
  assert.equal(c.$('audioState').textContent,'現在：音声OFF');
  assert.equal(b.textContent,'音声をONにする');
  assert.equal(b.attributes['aria-pressed'],'false');
  assert.equal(b.attributes['aria-busy'],'false');
  assert.equal(b.attributes['aria-label'],'現在：音声OFF。音声をONにする');
  assert.match(b.title,/現在：音声OFF。押すとON/);
  assert.equal(b.classes.has('active'),false);
  assert.equal(c.$('audioState').classes.has('active'),false);
  assert.equal(b.disabled,false);
}

test('initial control explicitly shows current OFF and future ON without creating audio',()=>{
  const c=setup();c.renderAudioControl();c.updateAudioCaption();assertOff(c);
  assert.equal(c.constructions,0);assert.equal(c.audioEnabled,false);
  assert.match(c.$('audioStatus').textContent,/スピーカー出力はOFF/);
  const html=fs.readFileSync(path.join(__dirname,'..','web','index.html'),'utf8');
  assert.match(html, /id="audioState"[^>]*aria-live="polite"[^>]*>現在：音声OFF/);
  assert.match(html, /id="audioButton"[^>]*aria-pressed="false"[^>]*>音声をONにする/);
});

test('successful user toggle shows current ON, next OFF and pressed state',async()=>{
  const c=setup();await c.toggleAudio();const b=c.$('audioButton');
  assert.equal(c.audioEnabled,true);assert.equal(c.constructions,1);assert.equal(c.resumes,1);
  assert.equal(c.$('audioState').textContent,'現在：音声ON');
  assert.equal(b.textContent,'音声をOFFにする');assert.equal(b.attributes['aria-pressed'],'true');
  assert.equal(b.attributes['aria-label'],'現在：音声ON。音声をOFFにする');
  assert.match(b.title,/現在：音声ON。押すとOFF/);
  assert.equal(b.classes.has('active'),true);assert.equal(c.$('audioState').classes.has('active'),true);
  assert.equal(c.audioSequence,0);assert.equal(b.disabled,false);
  assert.match(c.$('audioStatus').textContent,/VFO 2.*32\.0000 MHz.*音声待ち/);
});

test('turning OFF flushes queued sound and restores explicit OFF presentation',async()=>{
  const c=setup();await c.toggleAudio();await c.toggleAudio();assertOff(c);
  assert.equal(c.audioEnabled,false);assert.equal(c.flushes,1);
  assert.equal(c.constructions,1);assert.equal(c.resumes,1);
});

test('resume rejection stays OFF and permits a later manual retry',async()=>{
  let fail=true;const c=setup({resume:async ctx=>{if(fail)throw new Error('resume denied');ctx.state='running';}});
  await c.toggleAudio();assertOff(c);assert.equal(c.audioEnabled,false);assert.equal(c.flushes,1);
  assert.equal(c.errors.length,1);assert.match(c.errors[0].message,/音声を開始できません.*resume denied/);
  fail=false;await c.toggleAudio();assert.equal(c.audioEnabled,true);assert.equal(c.resumes,2);
  assert.equal(c.constructions,1);assert.equal(c.errors.length,1);
});

test('constructor failure never leaves an ON label or stuck busy control',async()=>{
  const c=setup({constructorError:true});await c.toggleAudio();assertOff(c);
  assert.equal(c.audioContext,null);assert.equal(c.audioEnabled,false);
  assert.equal(c.errors.length,1);assert.match(c.errors[0].message,/constructor failed/);
});

test('pending resume disables the button and ignores repeated clicks',async()=>{
  let finish;const c=setup({resume:ctx=>new Promise(resolve=>{finish=()=>{ctx.state='running';resolve();};})});
  const first=c.toggleAudio();
  assert.equal(c.audioEnabled,false);assert.equal(c.$('audioState').textContent,'現在：音声OFF');
  assert.equal(c.$('audioButton').textContent,'音声を開始中…');
  assert.equal(c.$('audioButton').disabled,true);assert.equal(c.$('audioButton').attributes['aria-busy'],'true');
  await c.toggleAudio();assert.equal(c.resumes,1);assert.equal(c.constructions,1);
  finish();await first;assert.equal(c.audioEnabled,true);assert.equal(c.audioStarting,false);
  assert.equal(c.$('audioButton').disabled,false);assert.equal(c.$('audioButton').attributes['aria-busy'],'false');
});

test('a resolved resume that is still suspended does not falsely claim ON',async()=>{
  const c=setup({resume:async()=>{}});await c.toggleAudio();assertOff(c);
  assert.equal(c.audioEnabled,false);assert.match(c.errors[0].message,/音声出力が停止/);
});

test('ON setting distinguishes stopped reception, connection wait and no selected VFO',async()=>{
  const c=setup();await c.toggleAudio();c.state.running=false;c.updateAudioCaption();
  assert.match(c.$('audioStatus').textContent,/受信・再生の開始待ち/);
  assert.equal(c.$('audioState').textContent,'現在：音声ON');
  c.connectionError=true;c.updateAudioCaption();assert.match(c.$('audioStatus').textContent,/接続待ち/);
  c.connectionError=false;c.state.running=true;c.selectedVfo=()=>null;c.updateAudioCaption();
  assert.match(c.$('audioStatus').textContent,/聴取するVFOを選択/);
});

test('pending audio start cannot turn sound back ON after application exit',async()=>{
  let finish;const c=setup({resume:ctx=>new Promise(resolve=>{finish=()=>{ctx.state='running';resolve();};})});
  const pending=c.toggleAudio();c.studioClosed=true;c.audioEnabled=false;
  finish();await pending;assert.equal(c.audioEnabled,false);assert.equal(c.audioStarting,false);
  assert.equal(c.errors.length,0);assert.equal(c.audioSequence,19);
});
