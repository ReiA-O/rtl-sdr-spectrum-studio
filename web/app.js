/* Local UI only. The receiver, validation, analysis and recording live on the server. */
(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  const escapeHtml = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const finite = (value, fallback = 0) => Number.isFinite(Number(value)) ? Number(value) : fallback;
  const clamp = (value, lo, hi) => Math.max(lo, Math.min(hi, value));
  const num = id => Number($(id).value);
  const mhz = hz => finite(hz) / 1e6;
  const colors = ['#64d7ec','#e6bf70','#bc9ce4','#75d6a9','#e99aa6','#8cafea','#d5d78b','#7cc6bf'];
  let state = null, frame = null, connectionError = false, actionInFlight = 0, preferredSource = null;
  let fileData = {files: [], presets: []}, vfoSignature = '', userMarkerSignature = '', drag = null, hover = null;
  let audioEnabled = false, audioStarting = false, audioContext = null, audioGain = null, audioStreamGain = null, audioSequence = 0, audioNextTime = 0;
  let audioEpoch = 0, audioRemoteEpoch = null, lastAudioConditions = '';
  let audioSources = [], lastAudioVfo = null, audioReceivedAt = 0;
  let layout = {spectrum_ratio: .6, vfo_width: 340, bottom_ratio: .5};
  let extraConditions = [], seenNotifications = new Set(), tuningTimer = null, frameFailures = 0, lastDeviceError = null;
  let frameTimer = null, framePending = false, frameRefreshRequested = false, frameEpoch = 0, traceResetsPending = 0;
  let actionQueue = Promise.resolve(), stateEpoch = 0;
  let studioClosing = false, studioClosed = false, stateTimer = null, audioTimer = null;
  let frontendChanging = false, frontendPendingValue = null;
  let userSelectionIntent, userSelectionGeneration = 0, userSelectionPending = false, userSelectionPromise = Promise.resolve(true);
  let vfoSelectionIntent, vfoSelectionGeneration = 0, vfoSelectionPromise = Promise.resolve(true), canvasGestureTarget = 'empty';
  let densityRangeDisplayContext = null, densityRangeEditContext = null, densityRangeEditGeneration = 0;
  let frequencyPan = null, frequencyPanPending = false;
  const dirtyForms = new Set();
  const vfoEdits = new Map();
  const receptionDefaults = Object.freeze({afc_enabled:false,afc_max_hz:1000,squelch_enabled:false,squelch_dbfs:-60,squelch_hysteresis_db:3,squelch_hang_ms:300,audio_gain_db:0,limiter_enabled:true,limiter_ceiling_dbfs:-2,click_suppression_enabled:true,wav_audio:'demod'});
  const receptionLimits = Object.freeze({afc_max_hz:{low:1,high:5000,label:'AFC補正上限'},squelch_dbfs:{low:-160,high:10,label:'RFしきい値'},squelch_hysteresis_db:{low:0,high:30,label:'ヒステリシス'},squelch_hang_ms:{low:0,high:5000,label:'保持時間'},audio_gain_db:{low:-24,high:36,label:'音声ゲイン'},limiter_ceiling_dbfs:{low:-24,high:0,label:'保護上限'}});

  function toast(message, type = 'info', duration = 4800) {
    if (!message) return;
    const el = document.createElement('div'); el.className = `toast ${type}`; el.textContent = String(message);
    $('toastRegion').append(el); setTimeout(() => el.remove(), duration);
    while ($('toastRegion').children.length > 4) $('toastRegion').firstElementChild.remove();
  }
  function notice(message, type = 'warning') {
    $('noticeText').textContent = String(message); $('noticeBar').className = `notice-bar ${type}`; $('noticeBar').hidden = !message;
  }
  function setValue(id, value, force = false) {
    const el = $(id); if (!el || value === undefined || value === null) return;
    if (!force && (document.activeElement === el || (el.form && dirtyForms.has(el.form.id)))) return;
    if (el.type === 'checkbox') el.checked = Boolean(value); else el.value = String(value);
  }
  function asText(id, value) { const el = $(id); if (el) el.textContent = value; }
  function dbText(value) { return Number.isFinite(Number(value)) && value !== null ? Number(value).toFixed(1) : '—'; }
  function formatHz(hz, digits = 2) {
    hz = finite(hz, NaN); if (!Number.isFinite(hz)) return '—';
    const abs = Math.abs(hz); return abs >= 1e6 ? `${(hz/1e6).toFixed(digits)} MHz` : abs >= 1e3 ? `${(hz/1e3).toFixed(digits)} kHz` : `${hz.toFixed(digits)} Hz`;
  }
  function formatBytes(bytes) { return bytes >= 1e9 ? `${(bytes/1e9).toFixed(2)} GB` : bytes >= 1e6 ? `${(bytes/1e6).toFixed(1)} MB` : bytes >= 1e3 ? `${(bytes/1e3).toFixed(1)} kB` : `${finite(bytes)} B`; }
  function inputLocked() { return Boolean(state && (state.locks?.input || state.trigger?.armed || state.recording?.iq_active || state.trigger?.recording)); }
  function frontendPresentation(current, disconnected = false) {
    const enabled=current?.frontend?.dc_reject_enabled,s=current?.frontend_status;
    if(typeof enabled!=='boolean')return {known:false,enabled:null,applied:false,label:'中心DC補正：状態未取得',kind:'',disabled:true,reason:'中心補正に対応したアプリとの接続を確認してください。'};
    const recorded=Boolean(current.locks?.frontend||current.locks?.input||current.trigger?.armed||current.trigger?.recording||current.recording?.iq_active||current.vfos?.some(v=>v.wav_recording));
    const reason=disconnected?'アプリとの接続を確認してください。':recorded?(current.locks?.frontend_reason||'IQ・WAVの保存とトリガ待機を終了してから中心補正を切り替えてください。'):'';
    const base={known:true,enabled,applied:false,label:'中心DC補正：状態確認中',kind:'',disabled:Boolean(reason),reason};
    if(disconnected)return {...base,label:'中心DC補正：接続待ち'};
    if(!s||s.enabled!==enabled||typeof s.applied!=='boolean'||typeof s.available!=='boolean')return {...base,disabled:true,reason:'中心補正の適用状態を確認しています。'};
    if(current.running!==true)return {...base,label:`中心DC補正：${enabled?'ON設定':'OFF'}（停止中）`};
    if(!enabled)return {...base,label:'中心DC補正 OFF'};
    if(s.available===false)return {...base,label:s.reason==='direct_sampling'?'中心DC補正：適用外（Direct入力）':s.reason==='sample_rate_unavailable'?'中心DC補正：適用外（サンプルレート）':'中心DC補正：適用外'};
    if(s.applied===true)return {...base,applied:true,label:'中心DC補正 ON',kind:'active'};
    return {...base,disabled:true,reason:'中心補正の適用状態を確認しています。'};
  }
  function frontendRouteKey(current) {
    if(typeof current?.frontend?.dc_reject_enabled!=='boolean')return null;
    return JSON.stringify([current.frontend.dc_reject_enabled,current.frontend_status?.available,current.frontend_status?.input_mode,current.source,current.input?.center_hz,current.input?.sample_rate]);
  }
  function frontendFrameMatchesState(current, incoming) {
    if(typeof current?.frontend?.dc_reject_enabled!=='boolean')return true;
    const status=incoming?.frontend_status,expected=current.frontend_status;
    if(!status||!expected||typeof status.applied!=='boolean'||typeof status.available!=='boolean'||typeof expected.applied!=='boolean'||typeof expected.available!=='boolean')return false;
    if(status.epoch!==undefined||expected.epoch!==undefined){if(!Number.isSafeInteger(status.epoch)||status.epoch<0||!Number.isSafeInteger(expected.epoch)||expected.epoch<0||status.epoch!==expected.epoch)return false;}
    return status.enabled===current.frontend.dc_reject_enabled&&status.available===expected.available&&status.input_mode===expected.input_mode&&status.applied===expected.applied;
  }
  function frontendDisplayRoute(current, snapshot, disconnected=false) {
    const view=frontendPresentation(current,disconnected);
    if(disconnected)return '表示状態は接続待ち';
    if(!view.known)return '補正状態は未取得';
    if(current.running!==true)return snapshot?.spectrum?.f_hz?.length?'停止中：表示は停止前の解析結果':'受信停止中';
    if(view.applied)return '中心補正後で探索・復調';
    if(view.enabled===false)return '中心補正OFFで探索・復調';
    if(current.frontend_status?.available===false)return view.label;
    return '中心補正の適用状態を確認中';
  }
  function renderFrontend() {
    const view=frontendPresentation(state,connectionError),toggle=$('dcRejectEnabled');
    toggle.checked=frontendChanging?frontendPendingValue===true:view.enabled===true;
    toggle.disabled=frontendChanging||view.disabled;toggle.setAttribute('aria-busy',String(frontendChanging));
    asText('frontendStatus',frontendChanging?'中心DC補正：切替中（適用待ち）':view.label);
    $('frontendStatus').className=`frontend-status ${!frontendChanging&&view.kind==='active'?'active':''}`;
    const reason=frontendChanging?'処理と表示を切り替えています。':view.reason;
    asText('frontendLockReason',reason);$('frontendLockReason').hidden=!reason;
    const route=frontendChanging?'補正状態を切替中':frontendDisplayRoute(state,frame,connectionError);
    asText('footerRight',`${route} · 原IQは未補正保存 · AM / NFM / WFM`);
  }
  async function changeFrontend() {
    const desired=$('dcRejectEnabled').checked===true,view=frontendPresentation(state,connectionError);
    if(frontendChanging||view.disabled){renderFrontend();if(view.reason)toast(view.reason,'warning');return null;}
    if(desired===view.enabled){renderFrontend();return null;}
    frontendChanging=true;frontendPendingValue=desired;renderFrontend();renderMeasurements(frame?.measurements || {});
    try{return await action('set_frontend',{frontend:{dc_reject_enabled:desired}},{message:desired?'中心DC補正をON設定にしました。原IQは未補正で保存します':'中心DC補正をOFFにしました'});}
    finally{frontendChanging=false;frontendPendingValue=null;renderFrontend();renderMeasurements(frame?.measurements || {});}
  }
  function peakTunePlan(current,vfo,frequency,disconnected=false) {
    if(!current||disconnected)return {error:'アプリとの接続を確認してください。'};
    if(!vfo)return {error:'選局するVFOを追加・選択してください。'};
    if(vfo.enabled===false)return {error:'選択中のVFOを再開してから選局してください。'};
    if(typeof frequency!=='number'||!Number.isFinite(frequency)||frequency<=0)return {error:'ピーク周波数を確認してください。'};
    const center=current.input?.center_hz,fs=current.input?.sample_rate,bw=vfo.bandwidth_hz;
    if(!Number.isFinite(center)||!Number.isFinite(fs)||fs<=0||!Number.isFinite(bw)||bw<=0)return {error:'受信帯域とVFO帯域幅を確認してください。'};
    const declared=current.input?.usable_bandwidth_hz,usable=Number.isFinite(declared)&&declared>0?Math.min(declared,fs*.9):fs*.9;
    if(Math.abs(frequency-center)+bw/2>usable/2)return {error:'このピークではVFO帯域が取得範囲を超えます。中心・スパンまたは復調帯域幅を確認してください。'};
    return {id:vfo.id,patch:{frequency_hz:frequency}};
  }
  function peakTuneButton(marker,label,compact=false) {
    const plan=peakTunePlan(state,selectedVfo(),marker?.frequency_hz,connectionError),dirty=selectedVfo()&&vfoEditState(selectedVfo().id).basic.dirty;
    const reason=frontendChanging?'中心補正の切替完了を待ってください。':dirty?'選択中VFOの未適用設定を先に適用してください。':plan.error;
    return `<button type="button" class="${compact?'peak-tune-chip':'button mini peak-tune-button'}" data-peak-key="${escapeHtml(label)}" data-peak-frequency="${escapeHtml(marker?.frequency_hz)}" data-peak-label="${escapeHtml(label)}" ${reason?'disabled':''} title="${escapeHtml(reason||`${label}の周波数へVFO ${selectedVfo()?.id}を同調`)}" aria-label="${escapeHtml(label)} ${mhz(marker?.frequency_hz).toFixed(6)} MHzへ選局">${compact?`<b>${escapeHtml(label)}</b>${mhz(marker.frequency_hz).toFixed(5)} · ${dbText(markerLevel(marker))}`:'選局'}</button>`;
  }
  async function tunePeakFrequency(frequency,label='ピーク') {
    if(frontendChanging){toast('中心補正の切替完了を待ってください。','warning');return null;}
    const vfo=selectedVfo(),planned=peakTunePlan(state,vfo,frequency,connectionError);
    if(planned.error){toast(planned.error,'warning');return null;}
    if(vfoEditState(vfo.id).basic.dirty){toast('選択中VFOの未適用設定を先に適用してください。','warning');return null;}
    cancelPendingVfoTune();const selection=activateVfo(vfo.id,{focus:false});
    const result=await action('update_vfo',{id:vfo.id,patch:planned.patch},{forceVfos:false,guard:async()=>{
      if(!await selection)return false;
      const current=state?.vfos?.find(item=>String(item.id)===String(vfo.id)),check=peakTunePlan(state,current,frequency,connectionError);
      if(check.error||vfoEditState(vfo.id).basic.dirty){toast(check.error||'VFOに新しい未適用設定があります。先に適用してください。','warning');return false;}
      return true;
    }});
    if(result){toast(`${label}へVFO ${vfo.id}を同調しました${state?.running?'':'。受信開始で復調します'}`);focusSpectrum();}
    return result;
  }
  function selectedVfo() { return (vfoSelectionIntent===undefined?null:state?.vfos?.find(v=>String(v.id)===String(vfoSelectionIntent))) || state?.vfos?.find(v => v.selected) || state?.vfos?.find(v => String(v.id) === String(state.selected_vfo_id)) || null; }
  function audioConditionsKey(vfo) { return JSON.stringify([state?.running,state?.source,vfo?.id,vfo?.frequency_hz,vfo?.bandwidth_hz,vfo?.mode,vfo?.enabled]); }
  function validAudioEpoch(value) { return Number.isSafeInteger(value)&&value>=0; }
  function syncStateAudioEpoch() {
    const epoch=state?.audio_epoch;if(!validAudioEpoch(epoch))return;
    if(validAudioEpoch(audioRemoteEpoch)&&epoch>audioRemoteEpoch){flushAudio();audioSequence=0;}
    if(!validAudioEpoch(audioRemoteEpoch)||epoch>audioRemoteEpoch)audioRemoteEpoch=epoch;
  }
  function audioPacketMatchesState(data) {
    if(data.vfo_id!==undefined&&data.vfo_id!==null&&String(data.vfo_id)!==String(selectedVfo()?.id))return false;
    if(data.epoch===undefined||data.epoch===null)return true;
    if(!validAudioEpoch(data.epoch))return false;
    return (!validAudioEpoch(state?.audio_epoch)||data.epoch>=state.audio_epoch)&&(!validAudioEpoch(audioRemoteEpoch)||data.epoch>=audioRemoteEpoch);
  }
  function wireId(id) { return state?.vfos?.find(v=>String(v.id)===String(id))?.id ?? id; }
  function linkedVfo(id) { return id === '' ? null : wireId(id); }
  function userMarkers() { return (state?.measurement?.manual_markers || []).filter(m=>m&&typeof m==='object'&&m.id!=null); }
  function selectedUserMarker() { const id=userSelectionIntent===undefined?state?.measurement?.selected_user_marker_id:userSelectionIntent;return id==null?null:userMarkers().find(m=>String(m.id)===String(id)) || null; }
  function userMarkerId(id) { return userMarkers().find(m=>String(m.id)===String(id))?.id ?? id; }
  function isUserMarker(marker) { return marker?.kind==='user'; }
  function markerLabel(marker,index=0) { return String(marker?.label || (isUserMarker(marker)?`U${marker.id}`:`A${marker?.rank || index+1}`)); }
  function userMarkerSelected(marker) { return isUserMarker(marker)&&String(marker.id)===String(selectedUserMarker()?.id); }
  function focusSpectrum() { $('spectrumCanvas').focus({preventScroll:true}); }
  function redrawMarkerControls() { renderMeasurements(frame?.measurements || {});drawSpectrum(); }
  function cancelPendingVfoTune() { clearTimeout(tuningTimer);delete $('spectrumCanvas').dataset.pendingFrequency;delete $('spectrumCanvas').dataset.pendingVfo; }
  function selectUserMarker(id, options = {}) {
    const target=id==null?null:userMarkerId(id),generation=++userSelectionGeneration;
    if(target!=null)cancelPendingVfoTune();
    userSelectionIntent=target;userSelectionPending=true;redrawMarkerControls();
    const operation=action('user_marker_select',{id:target},{forceVfos:false});
    const completion=operation.then(result=>{if(generation===userSelectionGeneration){userSelectionIntent=undefined;userSelectionPending=false;userSelectionPromise=Promise.resolve(true);redrawMarkerControls();if(result&&options.focus!==false)focusSpectrum();}return result;});
    userSelectionPromise=completion;return completion;
  }
  function activateVfo(id, options = {}) {
    const target=wireId(id),generation=++vfoSelectionGeneration,actual=state?.vfos?.find(v=>v.selected)?.id ?? state?.selected_vfo_id;
    const needsClear=Boolean(selectedUserMarker()||userSelectionPending||state?.measurement?.selected_user_marker_id!=null);
    vfoSelectionIntent=target;
    const cleared=needsClear?selectUserMarker(null,{focus:false}):Promise.resolve(true);
    const operation=String(actual)===String(target)?cleared:action('select_vfo',{id:target},{guard:()=>cleared});
    const completion=operation.then(result=>{if(generation===vfoSelectionGeneration){vfoSelectionIntent=undefined;vfoSelectionPromise=Promise.resolve(true);if(result&&options.focus!==false)focusSpectrum();}return result;});
    vfoSelectionPromise=completion;return completion;
  }
  async function userMarkerAction(name, extra = {}) { const marker=selectedUserMarker(),selection=userSelectionPromise;if(!marker)return null;return action(name,{id:marker.id,...extra},{forceVfos:false,guard:()=>selection}); }
  async function seekUserPeak(direction) { const result=await userMarkerAction('user_marker_peak',{direction});if(result)focusSpectrum(); }
  function levelUnit() { return state?.calibration?.unit === 'dBm' ? 'dBm' : 'dBFS'; }
  function markerLevel(marker) { return marker?.[levelUnit() === 'dBm' ? 'level_dbm' : 'level_dbfs']; }
  function validLevel(value) { return value !== null && value !== undefined && Number.isFinite(Number(value)); }
  function renderCalibration(force = false) {
    const c = state?.calibration || {}, absolute = c.valid === true && c.absolute_available === true, unit = levelUnit();
    const option = $('levelUnit').querySelector('[value="absolute"]'); option.disabled = !absolute;
    setValue('levelUnit', unit === 'dBm' ? 'absolute' : 'relative', force);
    $('calibrationClear').disabled = !c.loaded;
    $('calibrationStatus').className = `badge ${c.loaded && !absolute ? 'invalid' : absolute ? 'live' : ''}`;
    asText('calibrationStatus', absolute ? '有効（条件一致）' : c.loaded ? '条件不一致' : '未読込');
    asText('calibrationLabel', c.label || (c.loaded ? '校正情報を読み込み済み' : '校正情報は読み込まれていません'));
    asText('calibrationReason', c.reason || (absolute ? '絶対レベルへ切り替えられます。' : '相対レベルで表示します。'));
    asText('calibrationOffset', c.loaded && validLevel(c.offset_db) ? `${Number(c.offset_db).toFixed(3)} dB` : '—');
    asText('referenceUnit', unit);
    asText('levelUnitLabel', unit === 'dBm' ? '校正情報適用 dBm / RBW' : '相対 dBFS / RBW');
    asText('densityUnitLabel', `${unit} / RBW`); asText('waterfallUnitLabel', `${unit} / RBW`);
  }
  function densityLevelContext() {
    const unit=levelUnit(),offset=unit==='dBm'?state?.calibration?.offset_db:0;
    return {unit,offset_db:validLevel(offset)?Number(offset):null};
  }
  function clearDensityDisplayFrame(current) { return current?{...current,density:null}:current; }
  function renderDensityRange(force = false) {
    const context=densityLevelContext(),previous=densityRangeDisplayContext;
    const changed=previous&&(previous.unit!==context.unit||previous.offset_db!==context.offset_db);
    if(changed&&dirtyForms.has('densityRangeForm')){
      dirtyForms.delete('densityRangeForm');densityRangeEditContext=null;
      toast('表示単位・校正の変更により、Densityの未適用範囲を取り消しました','warning');
    }
    if(force){dirtyForms.delete('densityRangeForm');densityRangeEditContext=null;}
    densityRangeDisplayContext={...context};
    const valid=context.offset_db!==null;
    if(valid){setValue('densityMaxDb',finite(state?.analysis?.density_max_db,0)+context.offset_db,force||changed);setValue('densityMinDb',finite(state?.analysis?.density_min_db,-140)+context.offset_db,force||changed);}
    else{setValue('densityMaxDb','',true);setValue('densityMinDb','',true);}
    asText('densityMaxUnit',context.unit);asText('densityMinUnit',context.unit);
    $('densityRangeApply').disabled=!state||!valid;
    asText('densityRangeNote',valid?'変更時は密度履歴を再集計。Spectrum・Waterfall・受信は維持します。':'校正オフセットが未確認のため、範囲を適用できません。');
    if(changed){frameEpoch++;frame=clearDensityDisplayFrame(frame);requestFrame();}
  }
  function beginDensityRangeEdit() {
    if(!densityRangeEditContext&&densityRangeDisplayContext)densityRangeEditContext={...densityRangeDisplayContext};
    densityRangeEditGeneration++;dirtyForms.add('densityRangeForm');
  }
  function densityRangePayload(minimumText,maximumText,context) {
    if(typeof minimumText!=='string'||typeof maximumText!=='string'||!minimumText.trim()||!maximumText.trim())return {error:'Densityの上限・下限を両方入力してください。'};
    const minimum=Number(minimumText),maximum=Number(maximumText);
    if(!Number.isFinite(minimum)||!Number.isFinite(maximum)||maximum<=minimum)return {error:'Densityは有限の数値で、下限より大きい上限を指定してください。'};
    if(!context||!['dBFS','dBm'].includes(context.unit)||!validLevel(context.offset_db))return {error:'Densityの表示単位・校正条件を確認してください。'};
    const offset=context.unit==='dBm'?Number(context.offset_db):0,minDbfs=minimum-offset,maxDbfs=maximum-offset;
    if(!Number.isFinite(minDbfs)||!Number.isFinite(maxDbfs)||!Number.isFinite(maxDbfs-minDbfs))return {error:'Densityの範囲が大きすぎます。'};
    return {analysis:{density_min_db:minDbfs,density_max_db:maxDbfs,density_level_unit:'dBFS'}};
  }
  async function submitDensityRange(event) {
    event.preventDefault();
    if(!state||connectionError){toast('アプリとの接続を確認してください。','error');return null;}
    const payload=densityRangePayload($('densityMinDb').value,$('densityMaxDb').value,densityRangeEditContext || densityRangeDisplayContext);
    if(payload.error){toast(payload.error,'error');return null;}
    const generation=densityRangeEditGeneration;
    const changed=payload.analysis.density_min_db!==state.analysis?.density_min_db||payload.analysis.density_max_db!==state.analysis?.density_max_db;
    const result=await action('set_analysis',payload,{form:'densityRangeForm',formIsCurrent:()=>generation===densityRangeEditGeneration,forceVfos:false});
    if(result){if(generation===densityRangeEditGeneration)densityRangeEditContext=null;frameEpoch++;if(changed)frame=clearDensityDisplayFrame(frame);drawHeatmap('density');requestFrame();toast(dirtyForms.has('densityRangeForm')?'Densityの範囲を適用しました。追加の編集値は未適用です':'Densityの上下限を適用しました');}
    return result;
  }

  async function getJson(path) {
    const response = await fetch(path, {cache:'no-store'});
    const json = await response.json();
    if (!response.ok) throw new Error(json.error || `接続エラー (${response.status})`);
    return json;
  }
  function renderStudioLifecycle(){
    const button=$('shutdownButton'),panel=$('shutdownNotice');
    button.disabled=studioClosing||studioClosed;button.textContent=studioClosed?'終了済み':studioClosing?'終了処理中…':'アプリ終了';
    button.setAttribute('aria-busy',String(studioClosing));
    panel.hidden=!studioClosing&&!studioClosed;panel.className=`shutdown-notice${studioClosed?' completed':''}`;
    if(studioClosing)panel.textContent='進行中の操作を待ってから、アプリを終了します。';
    if(studioClosed)panel.textContent='終了しました。ブラウザーのタブを閉じてください。';
    document.body.classList.toggle('studio-closing',studioClosing);document.body.classList.toggle('studio-closed',studioClosed);
  }
  function shutdownConfirmation(snapshot){
    const recording=Boolean(snapshot?.recording?.iq_active||snapshot?.trigger?.recording||snapshot?.trigger?.armed||snapshot?.vfos?.some(v=>v.wav_recording));
    return `Studioを終了しますか？\n${recording?'IQ・WAVの録音、またはトリガ待機中です。保存を確定して終了します。':'受信・再生を停止して、アプリを終了します。'}\n未適用の入力・設定は適用されません。\n終了後はブラウザーのタブを閉じてください。`;
  }
  async function postStudioShutdown(){
    const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),15000);
    try{
      const response=await fetch('/api/action',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action:'shutdown'}),signal:controller.signal});
      const result=await response.json();
      if(!response.ok||result.ok!==true||result.shutdown!==true)throw new Error(result.error||result.message||`終了要求を確認できません (${response.status})`);
    }finally{clearTimeout(timer);}
  }
  async function waitForStudioStop(){
    for(let attempt=0;attempt<20;attempt++){
      const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),1000);
      try{await fetch('/api/health',{cache:'no-store',signal:controller.signal});}
      catch(error){if(error?.name==='AbortError')return false;return true;}
      finally{clearTimeout(timer);}
      await new Promise(resolve=>setTimeout(resolve,150));
    }
    return false;
  }
  async function shutdownStudio(){
    if(studioClosing||studioClosed)return false;
    studioClosing=true;renderStudioLifecycle();cancelPendingVfoTune();
    try{
      await actionQueue;
      const snapshot=await getJson('/api/state');
      if(!window.confirm(shutdownConfirmation(snapshot))){studioClosing=false;renderStudioLifecycle();return false;}
      asText('shutdownNotice','録音・受信処理を終了しています。完了までお待ちください。');
      await postStudioShutdown();
      if(!await waitForStudioStop())throw new Error('終了要求は送信しましたが、アプリの終了を確認できません。少し待って再操作してください。');
      studioClosed=true;studioClosing=false;frameEpoch++;frameRefreshRequested=false;
      clearTimeout(stateTimer);clearTimeout(frameTimer);clearTimeout(audioTimer);
      audioEnabled=false;audioStarting=false;flushAudio();renderAudioControl();updateAudioCaption();
      renderStudioLifecycle();
      for(const control of document.querySelectorAll('button,input,select'))control.disabled=true;
      for(const control of document.querySelectorAll('canvas,[role="separator"]'))control.setAttribute('aria-disabled','true');
      $('statusDot').className='status-dot';asText('sessionStatus','終了しました');asText('sourceBadge','終了');asText('deviceName','');
      asText('footerStatus','アプリは終了しました。ブラウザーのタブを閉じてください。');asText('footerRight','終了済み');
      return true;
    }catch(error){
      studioClosing=false;renderStudioLifecycle();
      const message=`終了を確認できませんでした：${error.message||error}`;
      $('shutdownNotice').hidden=false;$('shutdownNotice').className='shutdown-notice error';asText('shutdownNotice',message);toast(message,'error',6500);
      return false;
    }
  }
  function action(name, data = {}, options = {}) {
    if(studioClosing||studioClosed)return Promise.resolve(null);
    actionInFlight++;stateEpoch++;updatePresetLoadState();
    const operation=actionQueue.then(async()=>{if(options.guard&&!await options.guard())return null;return performAction(name,typeof data==='function'?data():data,options);});
    const completion=operation.finally(()=>{actionInFlight--;stateEpoch++;updatePresetLoadState();});
    actionQueue=completion.catch(()=>null);return completion;
  }
  async function performAction(name, data, options) {
    try {
      let response = await fetch('/api/action', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({action:name, ...data})});
      let json = await response.json();
      if ((!response.ok || json.ok === false) && name === 'set_input' && Array.isArray(json.impacted_vfos) && json.impacted_vfos.length && !data.confirm_impacted) {
        const names = json.impacted_vfos.map(v => typeof v === 'object' ? (v.name || `VFO ${v.id}`) : `VFO ${v}`).join('、');
        if (window.confirm(`中心周波数を変更すると、${names} が取得帯域外になるため無効になります。変更しますか？`)) {
          response = await fetch('/api/action', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({action:name, ...data, confirm_impacted:true})});
          json = await response.json();
        } else return null;
      }
      if (!response.ok || json.ok === false) throw new Error(json.error || json.message || `操作できません (${response.status})`);
      if (options.form && (!options.formIsCurrent || options.formIsCurrent())) dirtyForms.delete(options.form);
      if (json.state) applyState(json.state, Boolean(options.forceState), options.forceVfos !== false); else await refreshState(Boolean(options.forceState),true);
      requestFrame();
      if (options.message) toast(options.message);
      return json;
    } catch (error) {
      toast(error.message || String(error), 'error', 6500); return null;
    }
  }

  function updateLinks(id, selected, force) {
    const select = $(id); if (!select || (!force && (document.activeElement === select || (select.form && dirtyForms.has(select.form.id))))) return;
    const old = selected ?? select.value;
    const options = [{value:'', label:'独立した帯域'}, ...(state?.vfos || []).map(v => ({value:String(v.id), label:`VFO ${v.id} · ${mhz(v.frequency_hz).toFixed(4)} MHz`}))];
    select.replaceChildren(...options.map(o => { const el = document.createElement('option'); el.value=o.value; el.textContent=o.label; return el; }));
    select.value = old === null ? '' : String(old);
  }
  function applyState(next, force = false, forceVfos = false) {
    if (!next || typeof next !== 'object') return;
    const recovering=connectionError;
    const previousUnit = levelUnit(),previousFrontend=frontendRouteKey(state);
    const previousFrequency=JSON.stringify([state?.source,state?.input?.center_hz,state?.input?.sample_rate,state?.display?.span_hz,state?.input?.gain_db,state?.input?.agc,state?.input?.ppm,state?.input?.mode,state?.frontend_status?.epoch]);
    state = next; connectionError = false;
    const levelChanged = previousUnit !== levelUnit();
    if (levelChanged) { frame = null; requestFrame(); }
    if(previousFrontend!==frontendRouteKey(state)||previousFrequency!==JSON.stringify([state?.source,state?.input?.center_hz,state?.input?.sample_rate,state?.display?.span_hz,state?.input?.gain_db,state?.input?.agc,state?.input?.ppm,state?.input?.mode,state?.frontend_status?.epoch])){frameEpoch++;frame=null;cancelPendingVfoTune();drawAll();requestFrame();}
    if(recovering&&$('noticeText').textContent.startsWith('アプリとの接続が切れています'))notice('');
    const running = Boolean(state.running), replay = state.source === 'replay';
    const i = state.input || {}, a = state.analysis || {}, d = state.display || {}, t = state.trigger || {}, m = state.measurement || {};
    $('statusDot').className = `status-dot ${running ? 'running' : state.device?.error ? 'error' : ''}`;
    asText('sessionStatus', replay ? (running ? 'IQを再生中' : 'IQ再生・停止中') : running ? '受信中' : '停止中');
    $('sourceBadge').className = `badge ${replay ? 'replay' : running ? 'live' : ''}`;
    asText('sourceBadge', replay ? 'REPLAY' : state.source === 'demo' ? 'DEMO' : 'LIVE');
    asText('deviceName', state.device?.name || 'RTL-SDR');
    asText('receiveLabel', running ? '停止' : replay ? '再生開始' : '受信開始'); asText('receiveIcon', running ? '■' : '▶');
    $('receiveButton').className = `button ${running ? 'stopping' : 'primary'}`;
    $('sourceSelect').disabled = running || replay;
    if (!replay) { const source=state.source==='demo'?'demo':'live';if(running)preferredSource=source;setValue('sourceSelect',preferredSource || source,force); }
    setValue('centerMHz', mhz(i.center_hz), force); setValue('spanMHz', mhz(d.span_hz ?? i.sample_rate), force);
    setValue('fftMode', a.fft_mode || 'rbw', force); setValue('rbwHz', a.rbw_hz ?? a.target_rbw_hz ?? a.enbw_hz ?? 750, force);
    setValue('fftSize', a.fft_size, force); setValue('refLevel', d.ref_level ?? a.ref_level, force || levelChanged); setValue('rangeDb', d.range_db ?? a.range_db, force);
    setValue('gainDb', i.gain_db, force); setValue('agc', i.agc, force); setValue('ppm', i.ppm, force);
    setValue('sampleRate', i.sample_rate, force); setValue('inputMode', i.mode || 'auto', force);
    setValue('windowType', a.window, force); setValue('overlap', a.overlap, force); setValue('averageSeconds', a.average_seconds, force);
    setValue('detector', a.detector ?? d.detector, force); setValue('densityMetric', a.density_metric, force); setValue('waterfallRowSeconds', a.waterfall_row_seconds ?? .1, force);
    const traces = d.traces || {};
    for (const [key, id] of Object.entries({live:'traceLive',max:'traceMax',min:'traceMin',avg:'traceAvg'})) setValue(id, traces[key] ?? (key === 'live'), force);
    setValue('persistenceMode', a.persistence_mode, force); setValue('persistenceUnit', a.persistence_unit, force);
    setValue('persistenceSeconds', a.persistence_seconds, force); setValue('persistenceFrames', a.persistence_frames, force);
    setValue('switchPolicy', a.switch_policy, force); setValue('statsMode', a.stats_mode, force);
    updateLinks('measurementLink', m.link_vfo, force); updateLinks('triggerLink', t.link_vfo, force);
    setValue('measurementLowMHz', mhz(m.low_hz), force); setValue('measurementHighMHz', mhz(m.high_hz), force); setValue('obwPercent', m.obw_percent ?? 99, force);
    setValue('markerCount', m.marker_count ?? 6, force); setValue('markerMode', m.marker_mode || 'auto', force); setValue('markerFollow', m.marker_follow ?? true, force); setValue('deltaReference', m.delta_reference ?? 0, force);
    for (const [key,id] of Object.entries({mode:'triggerMode',metric:'triggerMetric',threshold_db:'thresholdDb',hysteresis_db:'hysteresisDb',pre_seconds:'preSeconds',post_seconds:'postSeconds',min_duration_s:'minDurationSeconds',rearm_s:'rearmSeconds',logic:'triggerLogic'})) setValue(id,t[key],force);
    setValue('triggerLowMHz', mhz(t.low_hz), force); setValue('triggerHighMHz', mhz(t.high_hz), force);
    if ((force || !dirtyForms.has('triggerForm')) && document.activeElement?.closest('#conditionList') === null) {
      const nextConditions = Array.isArray(t.conditions) ? t.conditions : [];
      if (JSON.stringify(extraConditions) !== JSON.stringify(nextConditions)) { extraConditions = nextConditions.map(c => ({...c})); renderConditions(); }
    }
    const locked = inputLocked(),centerLocked=locked || Boolean(state.locks?.center_sample_rate);
    for (const id of ['gainDb','agc','ppm','inputMode','inputApply']) $(id).disabled = locked || replay;
    for (const id of ['centerMHz','sampleRate']) $(id).disabled = centerLocked || replay;
    $('lockIndicator').hidden = !centerLocked;
    $('lockIndicator').textContent = locked ? 'IQ待機・保存中：入力条件をロック' : 'WAV録音中：中心周波数・サンプルレートをロック';
    const actual = i.gain_actual_db ?? i.effective_gain_db ?? state.device?.gain_db_actual;
    asText('gainActual', state.source==='demo' ? '実効 —（合成信号）' : i.agc ? '実効ゲイン：AGC' : actual==null ? '実効 —' : `${running?'実効':'最終実効'} ${finite(actual).toFixed(1)} dB`);
    asText('iqBadge', state.recording?.iq_active || t.recording ? '記録中' : t.armed ? 'ARMED' : '待機');
    $('iqBadge').className = `badge ${state.recording?.iq_active || t.recording ? 'recording' : ''}`;
    asText('iqButton', state.recording?.iq_active && !t.armed ? '■ IQ停止' : '● IQ保存');
    $('iqButton').disabled = t.armed || (!running && !state.recording?.iq_active);
    $('armButton').textContent = t.armed ? '待機解除' : 'トリガ待機'; $('armButton').classList.toggle('armed',Boolean(t.armed)); $('armButton').disabled = !running && !t.armed;
    asText('triggerState', t.state || (t.armed ? '待機中' : 'Disarmed'));
    asText('triggerPreAvailable', `プリバッファ ${finite(t.available_pre_seconds).toFixed(2)} s`);
    asText('triggerSummary', `${t.metric === 'integrated' ? 'Power' : 'Peak'} / ${finite(t.threshold_db,-55).toFixed(1)} dBFS · ${t.mode === 'auto' ? 'Auto' : 'One-Shot'}`);
    asText('recordingElapsed', state.recording?.elapsed_seconds != null ? `${finite(state.recording.elapsed_seconds).toFixed(1)} s` : '');
    asText('triggerCapacity', `確保済みプリトリガ ${finite(t.available_pre_seconds).toFixed(3)} s ／ 設定 ${finite(t.pre_seconds).toFixed(2)} s。${locked ? '入力条件はロック中です。' : 'Arm開始から入力条件をロックします。'}`);
    const perf = state.performance || {};
    asText('processingStatus', `処理 ${perf.load_percent != null ? finite(perf.load_percent).toFixed(0)+'%' : perf.processing_ms != null ? finite(perf.processing_ms).toFixed(1)+' ms' : '—'}`);
    asText('sampleCounter', `${finite(perf.valid_frames ?? perf.fft_frames ?? frame?.density?.valid_frames).toLocaleString()} FFT`);
    asText('gapReadout', `欠落 ${finite(perf.dropped_samples ?? perf.gap_samples).toLocaleString()} samples`);
    const actualMode=state.device?.mode_actual || i.mode;
    const modeLabel=actualMode==='direct_q'?'Direct Q':actualMode==='direct_i'?'Direct I':actualMode==='auto'?'入力モード自動':'チューナー受信';
    asText('footerStatus', state.device?.error ? `受信機：${state.device.error}` : replay ? '保存IQを再解析中 · ライブ受信とは排他' : running ? `${state.source === 'demo' ? '合成信号' : 'RTL-SDR'} · ${formatHz(state.device?.sample_rate ?? i.sample_rate,3).replace('Hz','S/s')} · ${i.mode==='auto'&&actualMode!=='auto'?'自動 → ':''}${modeLabel}` : '受信は停止中です');
    const provisional=String(state.device?.usable_bandwidth_status || '').startsWith('provisional');
    asText('receiverInfo', `${state.device?.name || 'RTL-SDR'} ／ ${state.device?.connected ? '接続確認済み' : '未接続または停止中'}。判定帯域 ${formatHz(state.device?.usable_bandwidth_hz ?? i.usable_bandwidth_hz ?? i.sample_rate,3)}${provisional?'（暫定）':''}。${modeLabel}。${locked ? '入力ロック中。' : ''}`);
    $('replayPanel').hidden = !replay;
    if (replay) {
      const r = state.replay || {}; asText('replayName', r.name || r.path || '保存IQ');
      $('replaySeek').max = finite(r.duration_seconds ?? r.total_seconds,1); setValue('replaySeek',r.position_seconds ?? 0,force);
      asText('replayPosition',`${finite(r.position_seconds).toFixed(2)} s`); asText('replayDuration',`${finite(r.duration_seconds ?? r.total_seconds).toFixed(2)} s`);
    }
    if (state.layout && !drag?.layout) applyLayout(state.layout, false);
    renderCalibration(force);renderDensityRange(force);renderVfos(force);renderMeasurements(frame?.measurements || {});renderFrontend();updateFftVisibility();updatePresetLoadState();
    const pv = a.persistence_mode === 'off' ? 'OFF' : a.persistence_mode === 'infinite' ? 'Infinite' : a.persistence_mode === 'window' ? '時間窓' : '指数減衰';
    asText('persistenceReadout',`${pv}${['off','infinite'].includes(a.persistence_mode) ? '' : ` · ${a.persistence_unit === 'frames' ? finite(a.persistence_frames).toLocaleString()+' FFT' : finite(a.persistence_seconds,1).toFixed(2)+' s'}`}`);
    for (const item of state.notifications || []) {
      const message = typeof item === 'string' ? item : item.message || item.text;
      const key = typeof item === 'string' ? item : item.id ?? `${item.timestamp || ''}:${message}`;
      if (!seenNotifications.has(key)) { seenNotifications.add(key); toast(message, item.level || item.type || 'info'); }
    }
    if (seenNotifications.size > 300) seenNotifications = new Set([...seenNotifications].slice(-150));
    if (state.device?.error) notice(state.device.error, 'error');
    else if(lastDeviceError&&$('noticeText').textContent===lastDeviceError)notice('');
    lastDeviceError=state.device?.error || null;
    const vfo = selectedVfo(),audioConditions=audioConditionsKey(vfo);
    if (audioConditions!==lastAudioConditions) { flushAudio();audioSequence=0;lastAudioVfo=vfo?.id;lastAudioConditions=audioConditions; }
    syncStateAudioEpoch();
    updateAudioCaption();
  }

  function vfoEditState(id) {
    const key=String(id);if(!vfoEdits.has(key))vfoEdits.set(key,{basic:{dirty:false,generation:0},reception:{dirty:false,generation:0}});return vfoEdits.get(key);
  }
  function markVfoEdited(event) {
    const field=event.target,card=field.closest('[data-vfo]');if(!card||(!field.dataset.vfoField&&!field.dataset.receptionField))return;
    const section=field.dataset.receptionField?'reception':'basic',edit=vfoEditState(card.dataset.vfo)[section];edit.dirty=true;edit.generation++;
    const status=card.querySelector(`[data-vfo-edit-status=${section}]`);if(status)status.textContent='未適用';
  }
  function receptionConfig(vfo) { return {...receptionDefaults,...(vfo?.reception || {})}; }
  function wavRouteLabel(route) { return route==='processed'?'聴取処理後':route==='demod'?'復調音（後処理なし）':'—'; }
  function readVfoReception(card) {
    const config={};
    for(const [key,initial] of Object.entries(receptionDefaults)){
      const field=card.querySelector(`[data-reception-field=${key}]`);if(!field)return {error:'受信・音声補助の入力欄を確認してください。'};
      if(typeof initial==='boolean'){config[key]=field.checked===true;continue;}
      if(typeof initial==='string'){if(!['demod','processed'].includes(field.value))return {error:'WAV保存経路を選択してください。'};config[key]=field.value;continue;}
      const text=field.value;if(typeof text!=='string'||!text.trim()||!Number.isFinite(Number(text)))return {error:'受信・音声補助の数値をすべて入力してください。'};
      config[key]=Number(text);
    }
    for(const [key,{low,high,label}] of Object.entries(receptionLimits))if(config[key]<low||config[key]>high)return {error:`${label}は ${low}〜${high} の範囲で指定してください。`};
    if(!Number.isInteger(config.squelch_hang_ms))return {error:'保持時間は整数のmsで指定してください。'};
    return {reception:config};
  }
  function readVfoBasic(card) {
    const field=key=>card.querySelector(`[data-vfo-field=${key}]`)?.value;
    const frequency=field('frequency'),bandwidth=field('bandwidth'),mode=field('mode'),format=field('wav-format');
    if(typeof frequency!=='string'||!frequency.trim()||typeof bandwidth!=='string'||!bandwidth.trim())return {error:'局周波数と復調帯域幅を入力してください。'};
    const hz=Number(frequency)*1e6,bw=Number(bandwidth)*1000;
    if(!Number.isFinite(hz)||hz<=0||!Number.isFinite(bw)||bw<=0||!['AM','NFM','WFM'].includes(mode)||!['PCM16','PCM24'].includes(format))return {error:'局周波数・帯域幅・復調方式・WAV形式を確認してください。'};
    return {patch:{frequency_hz:hz,bandwidth_hz:bw,mode,wav_format:format}};
  }
  async function applyVfoSection(card,vfo,section) {
    const payload=section==='reception'?readVfoReception(card):readVfoBasic(card);if(payload.error){toast(payload.error,'error');return null;}
    const edit=vfoEditState(vfo.id)[section],generation=edit.generation;
    const result=await action('update_vfo',{id:vfo.id,patch:section==='reception'?{reception:payload.reception}:payload.patch},{forceVfos:false});
    if(result){if(edit.generation===generation)edit.dirty=false;renderVfos();toast(edit.dirty?'設定を適用しました。追加の編集値は未適用です':section==='reception'?'受信・音声補助を適用しました':'VFO設定を適用しました');}
    return result;
  }
  function receptionReadout(vfo,running) {
    const config=receptionConfig(vfo),s=vfo.reception_status || {},available=running===true&&vfo.enabled!==false&&s.available===true&&!connectionError;
    const route=vfo.wav_recording?vfo.wav_summary?.route:config.wav_audio;
    const wav=`${vfo.wav_recording?'WAV記録中':'WAV保存'}：${wavRouteLabel(route)}`;
    if(!available){const reason=connectionError?'アプリとの接続待ち':!running?'受信停止中':vfo.enabled===false?'VFO停止中':s.reason==='gap'?'欠落後の音声待ち':'補助状態の更新待ち';return {available:false,afc:`AFC ${config.afc_enabled?'待機':'手動・補正OFF'} · 補正 — Hz`,rf:'RF — dBFS · SQL —',audio:'ピーク保護 — dB · 過大入力 — samples',state:reason,wav};}
    const afcNames={disabled:'OFF',qualifying:'判定中',tracking:'追従中',quality_insufficient:'信号不足',out_of_band:'範囲外',reference_outside_usable_band:'帯域端で保留',no_samples:'音声待ち'};
    const offset=validLevel(s.afc_offset_hz)?`${Number(s.afc_offset_hz)>=0?'+':''}${Number(s.afc_offset_hz).toFixed(1)}`:'—';
    const afc=`AFC ${config.afc_enabled?(afcNames[s.afc_status] || '判定中'):'手動・補正OFF'} · 補正 ${offset} Hz${config.afc_enabled&&s.afc_locked===true?' · LOCK':''}`;
    const rf=`RF ${s.rf_available===true?dbText(s.rf_dbfs):'—'} dBFS · SQL ${config.squelch_enabled?(s.squelch_open===true?'開':'閉'):'OFF'}`;
    const reduction=config.limiter_enabled?`${dbText(s.limiter_reduction_db)} dB`:'OFF';
    const overload=typeof s.pre_overload_samples==='number'&&Number.isFinite(s.pre_overload_samples)?Math.max(0,s.pre_overload_samples).toLocaleString():'—';
    return {available:true,afc,rf,audio:`ピーク保護 ${reduction} · 過大入力 ${overload} samples`,state:s.fade_active===true?'境界フェード中':'補助状態を更新中',wav};
  }
  function vfoReceptionHtml(vfo) {
    const c=receptionConfig(vfo),id=escapeHtml(vfo.id);
    const check=(key,label)=>`<label class="reception-check"><input type="checkbox" data-reception-field="${key}" ${c[key]===true?'checked':''} aria-label="VFO ${id} ${label}">${label}</label>`;
    const number=(key,label,unit,step='1')=>`<label>${label}<div class="input-unit"><input type="number" step="${step}" min="${receptionLimits[key].low}" max="${receptionLimits[key].high}" required data-reception-field="${key}" value="${escapeHtml(c[key])}" aria-label="VFO ${id} ${label}"><span>${unit}</span></div></label>`;
    return `<details class="vfo-reception"><summary>受信・音声補助 <span data-vfo-edit-status="reception"></span></summary><div class="reception-fields">${check('afc_enabled','AFC（自動周波数補正）')}${number('afc_max_hz','AFC補正上限','Hz')}${check('squelch_enabled','RFスケルチ')}${number('squelch_dbfs','RFしきい値','dBFS')}${number('squelch_hysteresis_db','ヒステリシス','dB', '0.1')}${number('squelch_hang_ms','保持時間','ms')}${number('audio_gain_db','音声ゲイン','dB','0.1')}${number('limiter_ceiling_dbfs','保護上限','dBFS','0.1')}${check('limiter_enabled','ピーク保護')}${check('click_suppression_enabled','クリック抑制')}<label class="reception-wav">WAV保存経路<select data-reception-field="wav_audio" aria-label="VFO ${id} WAV保存経路"><option value="demod" ${c.wav_audio!=='processed'?'selected':''}>復調音（後処理なし）</option><option value="processed" ${c.wav_audio==='processed'?'selected':''}>聴取処理後</option></select></label><div class="reception-note">AFCは両WAV経路に反映し、受信中心は動かしません。復調音は音声補助の前、聴取処理後は補助設定を反映（スピーカー音量は除外）。RF値・しきい値は常にdBFS。WAV経路・処理条件の変更は録音を分割します。</div><div class="reception-apply"><button class="button" data-vfo-action="reception-apply">補助に適用</button></div></div></details>`;
  }
  function vfoCardHtml(v,index) {
    const id=escapeHtml(v.id);
    return `<article class="vfo-card" data-vfo="${id}"><header class="vfo-card-header"><div class="vfo-name"><span class="vfo-color" style="background:${colors[index%colors.length]}"></span><strong>VFO ${id}</strong><span class="badge recording" data-vfo-rec hidden>REC</span></div><div class="vfo-header-actions"><button class="vfo-select" data-vfo-action="select">選択</button><button class="vfo-delete" data-vfo-action="delete" aria-label="VFO ${id}を削除">×</button></div></header><div class="vfo-fields"><label>局周波数（名目）<div class="input-unit"><input data-vfo-field="frequency" type="number" step="0.001" aria-label="VFO ${id} 周波数"><span>MHz</span></div></label><label>復調<select data-vfo-field="mode" aria-label="VFO ${id} 復調方式">${['AM','NFM','WFM'].map(mode=>`<option>${mode}</option>`).join('')}</select></label><label>復調帯域幅<div class="input-unit"><input data-vfo-field="bandwidth" type="number" step="0.1" min="0.1" aria-label="VFO ${id} 帯域幅"><span>kHz</span></div></label><label>状態<span class="vfo-state"></span></label></div><div class="vfo-actions"><button class="button" data-vfo-action="apply">適用</button><span class="vfo-edit-status" data-vfo-edit-status="basic"></span><button class="button record" data-vfo-action="record">● WAV</button><button class="button" data-vfo-action="resume" hidden>再開</button><select data-vfo-field="wav-format" aria-label="VFO ${id} WAV形式"><option>PCM16</option><option>PCM24</option></select></div><div class="vfo-reception-readout" aria-live="off"><div data-reception-readout="afc"></div><div data-reception-readout="rf"></div><div data-reception-readout="audio"></div><div data-reception-readout="wav"></div><span data-reception-readout="state"></span></div>${vfoReceptionHtml(v)}<div class="vfo-warning" hidden></div></article>`;
  }
  function syncVfoField(card,selector,value,protect,force=false) {
    const field=card.querySelector(selector);if(!field||protect||(!force&&document.activeElement===field))return;
    if(field.type==='checkbox')field.checked=value===true;else field.value=String(value);
  }
  function renderVfos(force = false) {
    const vfos=state?.vfos || [],list=$('vfoList');
    for(const card of list.querySelectorAll('[data-vfo]'))if(!vfos.some(v=>String(v.id)===card.dataset.vfo)){vfoEdits.delete(card.dataset.vfo);card.remove();}
    if(!vfos.length){list.innerHTML='<div class="vfo-empty">「＋ VFO」で受信する局を追加</div>';return;}
    list.querySelector('.vfo-empty')?.remove();
    vfos.forEach((v,index)=>{
      let card=Array.from(list.querySelectorAll('[data-vfo]')).find(el=>el.dataset.vfo===String(v.id));
      if(!card){list.insertAdjacentHTML('beforeend',vfoCardHtml(v,index));card=list.lastElementChild;}
      const edits=vfoEditState(v.id);if(force){edits.basic.dirty=false;edits.reception.dirty=false;}
      const selected=Boolean(v.selected||String(v.id)===String(state?.selected_vfo_id)),stopped=v.enabled===false,rec=Boolean(v.wav_recording);
      card.querySelector('.vfo-color').style.background=colors[index%colors.length];
      card.classList.toggle('selected',selected);card.classList.toggle('stopped',stopped);
      card.querySelector('.vfo-state').textContent=stopped?'停止':state?.running?'復調中':'待機（有効）';
      const select=card.querySelector('[data-vfo-action=select]');select.classList.toggle('selected',selected);select.textContent=selected?'選択中':'選択';
      const record=card.querySelector('[data-vfo-action=record]');record.disabled=!rec&&(stopped||!state?.running);record.textContent=rec?'■ WAV停止':'● WAV';record.classList.toggle('active',rec);
      card.querySelector('[data-vfo-rec]').hidden=!rec;card.querySelector('[data-vfo-action=resume]').hidden=!stopped;
      const values={frequency:mhz(v.frequency_hz).toFixed(6),bandwidth:(finite(v.bandwidth_hz)/1000).toFixed(2),mode:v.mode,'wav-format':v.wav_format || 'PCM16'};
      for(const [key,value] of Object.entries(values))syncVfoField(card,`[data-vfo-field=${key}]`,value,edits.basic.dirty&&!force,force);
      for(const [key,value] of Object.entries(receptionConfig(v)))if(key in receptionDefaults)syncVfoField(card,`[data-reception-field=${key}]`,value,edits.reception.dirty&&!force,force);
      for(const section of ['basic','reception'])card.querySelector(`[data-vfo-edit-status=${section}]`).textContent=edits[section].dirty?'未適用':'';
      const readout=receptionReadout(v,state?.running);for(const key of ['afc','rf','audio','state','wav'])card.querySelector(`[data-reception-readout=${key}]`).textContent=readout[key];
      card.querySelector('.vfo-reception-readout').classList.toggle('unavailable',!readout.available);
      const warning=card.querySelector('.vfo-warning');warning.hidden=!v.stopped_reason;warning.textContent=v.stopped_reason || '';
    });
  }
  async function vfoAction(event) {
    const button=event.target.closest('[data-vfo-action]');if(!button)return;
    const card=button.closest('[data-vfo]'),vfo=state?.vfos?.find(v=>String(v.id)===card.dataset.vfo);if(!vfo)return;
    const id=vfo.id;
    if(button.dataset.vfoAction!=='select'&&selectedUserMarker()&&!await selectUserMarker(null,{focus:false}))return;
    switch(button.dataset.vfoAction){
      case 'select':flushAudio();await activateVfo(id);break;
      case 'delete':if(!vfo.wav_recording||confirm(`VFO ${id} の録音を終了して削除しますか？`))await action('delete_vfo',{id});break;
      case 'resume':await action('resume_vfo',{id});break;
      case 'apply':await applyVfoSection(card,vfo,'basic');break;
      case 'reception-apply':await applyVfoSection(card,vfo,'reception');break;
      case 'record':
        if(vfo.wav_recording)await action('wav_stop',{id});
        else if(vfoEditState(id).basic.dirty||vfoEditState(id).reception.dirty)toast('未適用のVFO設定があります。各「適用」を押してからWAV保存を開始してください。','warning');
        else await action('wav_start',{id});
        break;
    }
  }

  function measurementMarkers(measurement = {}) {
    const received=Array.isArray(measurement.markers)?measurement.markers:[];
    return [...received.filter(m=>!isUserMarker(m)),...userMarkers().map(config=>{
      const measured=received.find(m=>isUserMarker(m)&&String(m.id)===String(config.id));
      const current=measured&&Math.abs(finite(measured.frequency_hz)-finite(config.frequency_hz))<.51?measured:null;
      return {...(current || {}),kind:'user',id:config.id,label:`U${config.id}`,frequency_hz:config.frequency_hz,level_dbfs:current?.level_dbfs ?? null,level_dbm:current?.level_dbm ?? null,reason:current?.reason || (current?'':'測定値の更新待ち')};
    })];
  }
  function renderUserMarkerControls(markers = []) {
    const users=userMarkers(),selected=selectedUserMarker(),unit=levelUnit();
    const signature=JSON.stringify(users.map(m=>({id:m.id,frequency_hz:m.frequency_hz,selected:String(m.id)===String(selected?.id)})));
    if(signature!==userMarkerSignature){
      userMarkerSignature=signature;
      $('userMarkerList').innerHTML=users.length?users.map(m=>`<button class="user-marker-chip ${String(m.id)===String(selected?.id)?'selected':''}" data-user-marker-select="${escapeHtml(m.id)}" aria-pressed="${String(m.id)===String(selected?.id)}" aria-label="ユーザMarker U${escapeHtml(m.id)} を選択" title="${mhz(m.frequency_hz).toFixed(6)} MHz">U${escapeHtml(m.id)}</button>`).join(''):'<span class="muted">自動ピーク A / ユーザ U</span>';
    }
    for(const id of ['userMarkerDeselect','userMarkerPeakLeft','userMarkerPeakRight','userMarkerPeakMax','userMarkerDelete'])$(id).disabled=!selected;
    const tunePlan=peakTunePlan(state,selectedVfo(),selected?.frequency_hz,connectionError),dirtyVfo=selectedVfo()&&vfoEditState(selectedVfo().id).basic.dirty;
    $('userMarkerTuneButton').disabled=!selected||frontendChanging||Boolean(tunePlan.error)||Boolean(dirtyVfo);
    $('userMarkerTuneButton').title=frontendChanging?'中心補正の切替完了を待ってください。':dirtyVfo?'選択中VFOの未適用設定を先に適用してください。':tunePlan.error||'この周波数へ選択中VFOを同調';
    $('userMarkerReadout').classList.toggle('selected',Boolean(selected));
    $('userMarkerReadout').setAttribute('aria-busy',String(userSelectionPending));
    if(selected){
      const measured=markers.find(m=>isUserMarker(m)&&String(m.id)===String(selected.id));
      const current=measured&&Math.abs(finite(measured.frequency_hz)-finite(selected.frequency_hz))<.51?measured:null;
      asText('userMarkerName',`U${selected.id}`);
      asText('userMarkerFrequency',`${mhz(selected.frequency_hz).toFixed(6)} MHz`);
      asText('userMarkerLevel',`${dbText(markerLevel(current))} ${unit}`);
      const offset=finite(selected.frequency_hz)-finite(state?.input?.center_hz);
      asText('userMarkerOffset',`中心から Δf ${offset>=0?'+':''}${(offset/1000).toFixed(3)} kHz`);
      asText('userMarkerStatus',userSelectionPending?'選択を切替中…':current?.reason || (current?'クリックで配置 · ← → は隣接Peak':'測定値の更新待ち'));
      asText('plotModeHint',`U${selected.id} 操作：クリックで配置 / ← → で隣接Peak　VFOの「選択」ボタンでVFO操作へ`);
    }else{
      asText('userMarkerName','U 未選択');asText('userMarkerFrequency','周波数 — MHz');asText('userMarkerLevel',`レベル — ${unit}`);asText('userMarkerOffset','中心から Δf — kHz');asText('userMarkerStatus','追加または U をクリックして選択');
      asText('plotModeHint','ピーク A のボタンで選局　VFO：ドラッグ / ホイール / ← →　Shiftクリック：U追加');
    }
  }
  function renderMeasurements(m = {}) {
    const activePeak=document.activeElement?.dataset?.peakKey,peakContainer=document.activeElement?.closest('#markerReadout')?'markerReadout':document.activeElement?.closest('#markerTable')?'markerTable':null;
    const unit=levelUnit(),absolute=unit==='dBm';
    $('channelPower').innerHTML=`${dbText(m[absolute?'cp_dbm':'cp_dbfs'])}<small>${unit}</small>`;
    $('channelPowerAverage').innerHTML=`${dbText(m[absolute?'cp_avg_dbm':'cp_avg_dbfs'])}<small>${unit}</small>`;
    $('obwValue').innerHTML=`${m.obw_hz!=null?(finite(m.obw_hz)/1000).toFixed(2):'—'}<small>kHz</small>`;
    asText('obwLabel',`OBW ${finite(state?.measurement?.obw_percent,99)}%`);
    const markers=measurementMarkers(m);
    const delta=$('deltaReference'),labels=markers.length?markers.map(markerLabel):['A1'],count=labels.length;
    if(Array.from(delta.options,o=>o.textContent).join('|')!==labels.join('|')&&document.activeElement!==delta){const chosen=delta.value;delta.replaceChildren(...labels.map((label,k)=>{const o=document.createElement('option');o.value=k;o.textContent=label;return o;}));delta.value=Number(chosen)<count?chosen:'0';}
    $('markerReadout').innerHTML=markers.filter(p=>!isUserMarker(p)).slice(0,8).map((p,k)=>peakTuneButton(p,markerLabel(p,k),true)).join('');
    const base=markers[finite(state?.measurement?.delta_reference)] || markers[0];
    $('markerTable').innerHTML=markers.length?`<table><thead><tr><th>Marker</th><th>MHz</th><th>${unit}</th><th>中心Δ kHz</th><th>基準Δ kHz</th><th>Δ dB</th><th>VFOへ</th></tr></thead><tbody>${markers.map((p,k)=>`<tr class="${isUserMarker(p)?'user-marker-row':''} ${userMarkerSelected(p)?'selected-marker-row':''}"><td>${isUserMarker(p)?`<button class="marker-table-select" data-user-marker-select="${escapeHtml(p.id)}" aria-pressed="${userMarkerSelected(p)}">${escapeHtml(markerLabel(p,k))}</button>`:escapeHtml(markerLabel(p,k))}</td><td>${mhz(p.frequency_hz).toFixed(6)}</td><td>${dbText(markerLevel(p))}</td><td>${((finite(p.frequency_hz)-finite(state?.input?.center_hz))/1000).toFixed(3)}</td><td>${base?((p.frequency_hz-base.frequency_hz)/1000).toFixed(3):'—'}</td><td>${base&&validLevel(markerLevel(p))&&validLevel(markerLevel(base))?(markerLevel(p)-markerLevel(base)).toFixed(2):'—'}</td><td>${peakTuneButton(p,markerLabel(p,k))}</td></tr>`).join('')}</tbody></table>`:'<p class="muted">受信後にMarker値を表示します。</p>';
    renderUserMarkerControls(markers);
    if(activePeak&&peakContainer){const replacement=Array.from($(peakContainer).querySelectorAll('[data-peak-key]')).find(button=>button.dataset.peakKey===activePeak);if(replacement&&!replacement.disabled)replacement.focus({preventScroll:true});}
  }

  function clearTraceDisplayFrame(current) {
    if(!current?.spectrum)return current;
    return {...current,spectrum:{...current.spectrum,max_db:[],min_db:[],avg_db:[]}};
  }
  function updateTraceResetButton() { $('traceResetButton').setAttribute('aria-busy',String(traceResetsPending>0)); }
  async function resetTraces() {
    traceResetsPending++;updateTraceResetButton();
    try {
      const result=await action('trace_reset',{}, {forceVfos:false});
      if(!result)return null;
      frameEpoch++;frame=clearTraceDisplayFrame(frame);drawSpectrum();requestFrame();
      toast('Max・Min・Averageをリセットしました');return result;
    }finally{traceResetsPending--;updateTraceResetButton();}
  }

  function canvasSize(canvas) {
    const rect=canvas.getBoundingClientRect(), ratio=Math.min(devicePixelRatio || 1,2);
    const width=Math.max(2,Math.round(rect.width)),height=Math.max(2,Math.round(rect.height));
    if(canvas.width!==Math.round(width*ratio)||canvas.height!==Math.round(height*ratio)){canvas.width=Math.round(width*ratio);canvas.height=Math.round(height*ratio);}
    const ctx=canvas.getContext('2d');ctx.setTransform(ratio,0,0,ratio,0,0);return {ctx,width,height};
  }
  function frequencyViewport(current, previewCenter = null) {
    const fs=Math.max(1,finite(current?.input?.sample_rate,2048000));
    const requested=finite(current?.display?.span_hz,fs),span=requested>0?Math.min(requested,fs):fs;
    const center=Number.isFinite(previewCenter)?previewCenter:finite(current?.input?.center_hz,32256000);
    return {center,span,low:center-span/2,high:center+span/2};
  }
  function plotGeometry(width,height,levelAxis=true) {
    const p={left:levelAxis?54:43,right:13,top:15,bottom:29,width:0,height:0};
    p.width=Math.max(1,Math.round(width)-p.left-p.right);p.height=Math.max(1,Math.round(height)-p.top-p.bottom);return p;
  }
  function heatmapFrequencyBounds(spectrum,width) {
    const frequencies=spectrum?.f_hz;
    if(!Array.isArray(frequencies)||frequencies.length!==width||!frequencies.every(Number.isFinite))return null;
    if(width===1){const step=finite(spectrum.bin_hz,NaN);return step>0?{low:frequencies[0]-step/2,high:frequencies[0]+step/2}:null;}
    for(let i=1;i<width;i++)if(frequencies[i]<=frequencies[i-1])return null;
    const step=(frequencies[width-1]-frequencies[0])/(width-1);
    return {low:frequencies[0]-step/2,high:frequencies[width-1]+step/2};
  }
  function plotAxes(canvas, levelAxis = true, limits = null) {
    const s=canvasSize(canvas),{ctx,width,height}=s;
    const p=plotGeometry(width,height,levelAxis),view=frequencyViewport(state,frequencyPan?.preview_center_hz);
    const {low,high,span}=view,top=limits?limits.top:finite(state?.display?.ref_level,state?.analysis?.ref_level ?? -20),range=limits?limits.range:finite(state?.display?.range_db,state?.analysis?.range_db ?? 100);
    ctx.fillStyle='#0b141c';ctx.fillRect(0,0,width,height);ctx.font='9px Consolas,monospace';ctx.lineWidth=1;ctx.strokeStyle='#20323f';ctx.fillStyle='#668499';
    for(let k=0;k<=8;k++){const x=p.left+p.width*k/8;ctx.beginPath();ctx.moveTo(x+.5,p.top);ctx.lineTo(x+.5,p.top+p.height);ctx.stroke();ctx.textAlign='center';ctx.fillText(((low+span*k/8)/1e6).toFixed(span<100000?4:3),x,height-11);}
    if(levelAxis) for(let k=0;k<=5;k++){const y=p.top+p.height*k/5;ctx.beginPath();ctx.moveTo(p.left,y+.5);ctx.lineTo(p.left+p.width,y+.5);ctx.stroke();ctx.textAlign='right';ctx.fillText((top-range*k/5).toFixed(0),p.left-8,y+3);}
    ctx.textAlign='right';ctx.fillStyle='#4f7085';ctx.fillText('MHz',width-5,height-1);
    return {...s,p,low,high,span,top,range,x:hz=>p.left+(hz-low)/span*p.width,y:db=>p.top+(top-db)/range*p.height};
  }
  function drawSpectrum() {
    const c=$('spectrumCanvas'),s=plotAxes(c),{ctx,p}=s;
    const spectrum=frame?.spectrum || {},freq=spectrum.f_hz || [];
    $('spectrumEmpty').hidden=Boolean(freq.length);
    const measurements=state?.measurement || {};
    const roiLow=finite(measurements.low_hz,NaN),roiHigh=finite(measurements.high_hz,NaN);
    ctx.save();ctx.beginPath();ctx.rect(p.left,p.top,p.width,p.height);ctx.clip();
    if(Number.isFinite(roiLow)&&Number.isFinite(roiHigh)){ctx.fillStyle='#507a9b12';ctx.fillRect(s.x(roiLow),p.top,s.x(roiHigh)-s.x(roiLow),p.height);ctx.strokeStyle='#678ea04d';ctx.setLineDash([3,4]);for(const f of [roiLow,roiHigh]){ctx.beginPath();ctx.moveTo(s.x(f),p.top);ctx.lineTo(s.x(f),p.top+p.height);ctx.stroke();}ctx.setLineDash([]);}
    (state?.vfos || []).forEach((v,k)=>{
      let f=finite(v.frequency_hz),bw=finite(v.bandwidth_hz);if(drag?.id===String(v.id)){f=drag.frequency_hz;bw=drag.bandwidth_hz;}
      const selected=Boolean(v.selected||String(v.id)===String(state?.selected_vfo_id)),color=colors[k%colors.length];
      ctx.fillStyle=color+(selected?'1d':'0a');ctx.fillRect(s.x(f-bw/2),p.top,s.x(f+bw/2)-s.x(f-bw/2),p.height);
      ctx.strokeStyle=color+(v.enabled===false?'55':selected?'dd':'77');ctx.lineWidth=selected?1.3:1;
      for(const edge of [f-bw/2,f+bw/2]){ctx.beginPath();ctx.moveTo(s.x(edge),p.top);ctx.lineTo(s.x(edge),p.top+p.height);ctx.stroke();if(selected){ctx.fillStyle=color;ctx.fillRect(s.x(edge)-2,p.top+27,4,11);}}
      ctx.beginPath();ctx.moveTo(s.x(f),p.top);ctx.lineTo(s.x(f),p.top+p.height);ctx.stroke();ctx.fillStyle=color;ctx.font='10px Consolas,monospace';ctx.textAlign='center';ctx.fillText(`V${v.id}`,s.x(f),p.top+11);
    });
    const traces=state?.display?.traces || {live:true};
    for(const [key,field,color] of [['min','min_db','#a896df'],['max','max_db','#e9c16a'],['avg','avg_db','#d4e6ed'],['live','live_db','#5ad7e8']]) {
      if(!traces[key] || !Array.isArray(spectrum[field]) || !freq.length)continue;
      ctx.beginPath();ctx.strokeStyle=color;ctx.lineWidth=key==='live'?1.15:1;let started=false;
      spectrum[field].forEach((db,k)=>{const f=freq[k];if(f==null||db==null||!Number.isFinite(db)){started=false;return;}const x=s.x(f),y=clamp(s.y(db),p.top-10,p.top+p.height+10);if(!started){ctx.moveTo(x,y);started=true;}else ctx.lineTo(x,y);});ctx.stroke();
    }
    const markers=measurementMarkers(frame?.measurements).sort((a,b)=>Number(userMarkerSelected(a))-Number(userMarkerSelected(b)));ctx.font='9px Consolas,monospace';
    markers.forEach((m,k)=>{
      const db=markerLevel(m),x=s.x(m.frequency_hz),user=isUserMarker(m),selected=userMarkerSelected(m),label=markerLabel(m,k);
      if(user){
        ctx.strokeStyle=selected?'#ffa4df':'#c087c388';ctx.lineWidth=selected?1.5:1;ctx.setLineDash(selected?[]:[4,4]);ctx.beginPath();ctx.moveTo(x,p.top);ctx.lineTo(x,p.top+p.height);ctx.stroke();ctx.setLineDash([]);
        const width=ctx.measureText(label).width+10;ctx.fillStyle=selected?'#f3a3d8':'#493448';ctx.fillRect(x-width/2,p.top+17,width,15);ctx.fillStyle=selected?'#25131f':'#f1c4e3';ctx.textAlign='center';ctx.fillText(label,x,p.top+28);
      }
      if(!validLevel(db))return;
      const y=clamp(s.y(db),p.top+(user?43:20),p.top+p.height-3);ctx.fillStyle=user?(selected?'#ffd0ee':'#d59bce'):'#f3d08a';ctx.beginPath();ctx.moveTo(x,y-1);ctx.lineTo(x-4,y-8);ctx.lineTo(x+4,y-8);ctx.closePath();ctx.fill();
      if(!user){ctx.textAlign='center';ctx.fillText(label,x,y-12);}
    });
    if(hover && !drag){ctx.strokeStyle='#7da2b350';ctx.setLineDash([3,3]);ctx.beginPath();ctx.moveTo(hover.x,p.top);ctx.lineTo(hover.x,p.top+p.height);ctx.stroke();ctx.setLineDash([]);}
    ctx.restore();
    const panReason=frequencyPanPolicy(state,connectionError,dirtyForms.has('tuningForm'));
    asText('spanReadout',`${frequencyPan?'中心プレビュー · ':frequencyPanPending?'中心変更中 · ':'↔ '}${(s.low/1e6).toFixed(4)} — ${(s.high/1e6).toFixed(4)} MHz`);
    $('spanReadout').title=panReason||'左右にドラッグして受信中心を移動（スパンとVFO周波数は固定）。← → キーでも移動できます。';
    $('spanReadout').setAttribute('aria-disabled',String(Boolean(panReason)||frequencyPanPending));
    $('spanReadout').setAttribute('aria-valuenow',String(frequencyPan?.preview_center_hz ?? state?.input?.center_hz ?? 0));
    $('spanReadout').setAttribute('aria-valuetext',`${mhz(frequencyPan?.preview_center_hz ?? state?.input?.center_hz).toFixed(6)} MHz`);
    $('spanReadout').setAttribute('aria-valuemin',String(state?.input?.mode==='tuner'?24000000:10000));
    $('spanReadout').setAttribute('aria-valuemax',String(['direct_i','direct_q'].includes(state?.input?.mode)?28800000:1766000000));
    $('spanReadout').classList.toggle('frequency-panning',Boolean(frequencyPan));
    return s;
  }
  const palette=new Uint8ClampedArray(256*4);
  const stops=[[0,[9,17,27]],[.12,[31,29,72]],[.3,[49,68,142]],[.48,[24,143,169]],[.65,[57,197,158]],[.8,[205,211,87]],[.92,[239,144,64]],[1,[255,221,151]]];
  for(let k=0;k<256;k++){const f=k/255;let index=1;while(index<stops.length-1&&stops[index][0]<f)index++;const a=stops[index-1],b=stops[index],q=(f-a[0])/(b[0]-a[0]);for(let j=0;j<3;j++)palette[k*4+j]=Math.round(a[1][j]+(b[1][j]-a[1][j])*q);palette[k*4+3]=255;}
  const heatmaps={density:document.createElement('canvas'),waterfall:document.createElement('canvas')};
  function decodedBytes(encoded) { const raw=atob(encoded || '');return Uint8Array.from(raw,c=>c.charCodeAt(0)); }
  function densityPlotLimits(data) {
    if(Number.isFinite(data?.min_db)&&Number.isFinite(data?.max_db)&&data.max_db>data.min_db)return {top:data.max_db,range:data.max_db-data.min_db};
    const minimum=finite(state?.analysis?.density_min_db,-140),maximum=finite(state?.analysis?.density_max_db,0),offset=densityLevelContext().offset_db ?? 0;
    return {top:maximum+offset,range:maximum-minimum};
  }
  function renderDensityHistory(data = {}) {
    const windowMode=['window','time_window'].includes(state?.analysis?.persistence_mode);
    const el=$('densityHistory');el.hidden=!windowMode&&!data.window_truncated;
    if(el.hidden)return;
    const retained=Number.isFinite(data.window_frames_retained)?`${data.window_frames_retained.toLocaleString()} FFT`:'— FFT';
    const seconds=Number.isFinite(data.window_seconds_retained)?`${data.window_seconds_retained.toFixed(3)} s / `:'';
    const usage=Number.isFinite(data.window_history_bytes)?` · ${formatBytes(data.window_history_bytes)}`:'';
    const capacity=Number.isFinite(data.window_capacity_seconds)?`（連続解析 ${data.window_capacity_seconds.toFixed(3)} s 目安）`:'';
    const limit=Number.isFinite(data.window_history_limit_bytes)?` / 上限 ${formatBytes(data.window_history_limit_bytes)}${capacity}`:'';
    const warning=data.window_truncated?'メモリ上限により時間窓を短縮 · ':data.window_memory_limit_expected?'指定窓が保持容量の目安を超過 · ':'';
    el.className=`density-history${warning?' limited':''}`;
    el.textContent=`${warning}実効保持 ${seconds}${retained}${usage}${limit}`;
  }
  function drawHeatmap(kind) {
    const canvas=$(kind==='density'?'densityCanvas':'waterfallCanvas'),data=frame?.[kind];
    const limits=kind==='density'?densityPlotLimits(data):null;
    const s=plotAxes(canvas,kind==='density',limits);
    if(!data?.data_b64||!data.width||!data.height)return;
    const w=Math.trunc(data.width),h=Math.trunc(data.height);if(w*h>4194304||w<1||h<1)return;
    const frequencyBounds=heatmapFrequencyBounds(frame?.spectrum,w);if(!frequencyBounds)return;
    let values;try{values=decodedBytes(data.data_b64);}catch{return;}if(values.length<w*h)return;
    const off=heatmaps[kind];off.width=w;off.height=h;const ctx=off.getContext('2d'),image=ctx.createImageData(w,h);
    for(let y=0;y<h;y++)for(let x=0;x<w;x++){const sy=data.invert_y||data.orientation==='bottom_high'?h-1-y:y;const v=values[sy*w+x],at=(y*w+x)*4;image.data[at]=palette[v*4];image.data[at+1]=palette[v*4+1];image.data[at+2]=palette[v*4+2];image.data[at+3]=255;}
    ctx.putImageData(image,0,0);s.ctx.save();s.ctx.beginPath();s.ctx.rect(s.p.left,s.p.top,s.p.width,s.p.height);s.ctx.clip();s.ctx.imageSmoothingEnabled=false;
    const x1=s.x(frequencyBounds.low),x2=s.x(frequencyBounds.high);
    if(kind==='density') {
      const top=finite(data.max_db,s.top),bottom=finite(data.min_db,s.top-s.range),y1=s.y(top),y2=s.y(bottom);
      s.ctx.drawImage(off,x1,y1,x2-x1,y2-y1);
    } else {
      s.ctx.drawImage(off,x1,s.p.top,x2-x1,s.p.height);
      if(Array.isArray(data.gap_rows))for(const row of data.gap_rows){const y=s.p.top+row/h*s.p.height,height=Math.max(2,s.p.height/h);s.ctx.fillStyle='#566471';s.ctx.fillRect(x1,y,x2-x1,height);s.ctx.strokeStyle='#b0bdc477';for(let x=x1;x<x2;x+=11){s.ctx.beginPath();s.ctx.moveTo(x,y+height);s.ctx.lineTo(x+height,y);s.ctx.stroke();}}
    }
    s.ctx.restore();
    if(kind==='density') {
      asText('densityStats',`有効FFT ${finite(data.valid_frames).toLocaleString()}`);
      const max=data.maximum_value;
      asText('densityScale',Number.isFinite(max)?`相対色 · 最大 ${data.metric==='rate'?(max*100).toFixed(1)+'%':max.toFixed(data.weighted_hits?1:0)+(data.weighted_hits?' 相当':' hits')}`:data.metric==='rate'?'出現率':'ヒット数');
      $('densityScale').title=`色は表示中の相対強度。${data.weighted_hits?'指数減衰した値は重み付き相当値。':''}${data.window_truncated?'履歴のメモリー上限に達しています。':''}`;
    }
    else {const seconds=finite(data.row_seconds,.1)*h;asText('waterfallTime',`${seconds.toFixed(1)} s`);}
  }
  function drawAll() { drawSpectrum();drawHeatmap('density');drawHeatmap('waterfall'); }
  function acceptFrame(next) {
    if(!frontendFrameMatchesState(state,next))return;
    const unit=next.spectrum?.unit || next.unit;
    if(unit && (String(unit).startsWith('dBm') !== (levelUnit()==='dBm')))return;
    frame=next;renderMeasurements(next.measurements || {});renderDensityHistory(next.density || {});renderFrontend();drawAll();
    const a=state?.analysis || {},s=next.spectrum || {};
    const fft=a.fft_size || next.fft_size,rbw=next.enbw_hz ?? s.enbw_hz ?? a.enbw_hz,bin=next.bin_hz ?? s.bin_hz ?? a.bin_hz;
    asText('fftReadout',`FFT ${fft ? finite(fft).toLocaleString():'—'} · RBW ${rbw ? formatHz(rbw,1):'—'}${bin?' · bin '+formatHz(bin,1):''}`);
  }

  function plotFrequency(event) { const rect=$('spectrumCanvas').getBoundingClientRect(),s=plotAxesGeometry();return {x:event.clientX-rect.left,y:event.clientY-rect.top,hz:s.low+(event.clientX-rect.left-s.left)/s.width*s.span,s}; }
  function plotAxesGeometry(){const rect=$('spectrumCanvas').getBoundingClientRect();return {...plotGeometry(rect.width,rect.height),...frequencyViewport(state,frequencyPan?.preview_center_hz)};}
  function frequencyPanContext(current) {
    return JSON.stringify([current?.source,current?.input?.center_hz,current?.input?.sample_rate,current?.input?.mode,current?.display?.span_hz]);
  }
  function frequencyPanPolicy(current,disconnected=false,dirty=false) {
    if(!current||disconnected)return 'アプリとの接続を確認してください。';
    if(current.source==='replay')return '保存IQの受信中心は変更できません。';
    if(current.locks?.input||current.locks?.center_sample_rate||current.trigger?.armed||current.trigger?.recording||current.recording?.iq_active||current.vfos?.some(v=>v.wav_recording))return 'IQ・WAV保存とトリガ待機を終了してから中心を移動してください。';
    if(dirty)return '未適用の周波数・表示設定を先に適用してください。';
    if(!Number.isFinite(current.input?.center_hz)||!Number.isFinite(current.input?.sample_rate)||current.input.sample_rate<=0)return '受信中心とサンプルレートを確認してください。';
    return '';
  }
  function frequencyPanPlan(current,gesture,clientX,disconnected=false,dirty=false) {
    const error=frequencyPanPolicy(current,disconnected,dirty);if(error)return {error};
    if(!gesture||frequencyPanContext(current)!==gesture.context)return {error:'受信条件が変わったため、中心移動を取り消しました。'};
    if(!Number.isFinite(clientX)||!Number.isFinite(gesture.start_x)||!Number.isFinite(gesture.center)||!Number.isFinite(gesture.width)||gesture.width<=0||!Number.isFinite(gesture.span)||gesture.span<=0)return {error:'周波数軸の幅を確認してください。'};
    const mode=current.input.mode,minimum=mode==='tuner'?24000000:10000,maximum=['direct_i','direct_q'].includes(mode)?28800000:1766000000;
    const proposed=gesture.center-(clientX-gesture.start_x)/gesture.width*gesture.span;
    if(!Number.isFinite(proposed))return {error:'周波数軸の移動量を確認してください。'};
    const center=Math.round(clamp(proposed,minimum,maximum));
    return {center_hz:center,span_hz:gesture.span,changed:center!==gesture.center,bounded:proposed<minimum||proposed>maximum};
  }
  function beginFrequencyPan(event,footer=false) {
    if(event.button!==0)return false;
    const geometry=plotAxesGeometry(),rect=$('spectrumCanvas').getBoundingClientRect();
    const x=event.clientX-rect.left,y=event.clientY-rect.top;
    if(!footer&&(x<geometry.left||x>geometry.left+geometry.width||y<geometry.top+geometry.height||y>rect.height))return false;
    canvasGestureTarget='axis';event.preventDefault();
    const reason=frequencyPanPolicy(state,connectionError,dirtyForms.has('tuningForm'));
    if(reason||frequencyPanPending||frontendChanging||drag){toast(reason||'ほかの操作の完了を待ってください。','warning');return true;}
    cancelPendingVfoTune();hover=null;$('cursorReadout').hidden=true;
    frequencyPan={context:frequencyPanContext(state),center:state.input.center_hz,span:geometry.span,width:geometry.width,start_x:event.clientX,preview_center_hz:state.input.center_hz,capture:event.currentTarget||$('spectrumCanvas'),pointer_id:event.pointerId};
    frequencyPan.capture.setPointerCapture(event.pointerId);$('spectrumCanvas').style.cursor='grabbing';$('spanReadout').focus({preventScroll:true});drawAll();return true;
  }
  function frequencyAxisHover(event) {
    const geometry=plotAxesGeometry(),canvas=$('spectrumCanvas'),rect=canvas.getBoundingClientRect();
    const x=event.clientX-rect.left,y=event.clientY-rect.top;
    const inside=x>=geometry.left&&x<=geometry.left+geometry.width&&y>=geometry.top+geometry.height&&y<=rect.height;
    canvas.style.cursor=inside?(frequencyPanPolicy(state,connectionError,dirtyForms.has('tuningForm'))?'not-allowed':'grab'):'';
    if(inside){hover=null;$('cursorReadout').hidden=true;drawSpectrum();}return inside;
  }
  function moveFrequencyPan(event) {
    if(!frequencyPan)return false;
    const plan=frequencyPanPlan(state,frequencyPan,event.clientX,connectionError,dirtyForms.has('tuningForm'));
    if(plan.error){cancelFrequencyPan();toast(plan.error,'warning');return true;}
    frequencyPan.preview_center_hz=plan.center_hz;event.preventDefault();drawAll();return true;
  }
  function cancelFrequencyPan() {
    if(!frequencyPan)return;
    const previous=frequencyPan;frequencyPan=null;try{previous.capture.releasePointerCapture(previous.pointer_id);}catch{}$('spectrumCanvas').style.cursor='';drawAll();
  }
  async function finishFrequencyPan(event) {
    if(!frequencyPan)return false;
    const previous=frequencyPan,plan=frequencyPanPlan(state,previous,event.clientX,connectionError,dirtyForms.has('tuningForm'));cancelFrequencyPan();
    if(plan.error){toast(plan.error,'warning');return true;}if(!plan.changed)return true;
    await commitFrequencyPan(previous,plan.center_hz);return true;
  }
  async function commitFrequencyPan(previous,center) {
    frequencyPanPending=true;drawAll();
    try{return await action('set_input',{input:{center_hz:center},confirm_impacted:true},{forceVfos:false,message:'受信中心を移動しました。スパンとVFO周波数は固定し、帯域外のVFOは停止します',guard:()=>{
      const reason=frequencyPanPolicy(state,connectionError,dirtyForms.has('tuningForm'));
      if(reason||frontendChanging||frequencyPanContext(state)!==previous.context){toast(reason||'受信条件が変わったため、中心移動を取り消しました。','warning');return false;}return true;
    }});}finally{frequencyPanPending=false;drawAll();}
  }
  async function frequencyPanKey(event) {
    if(!['ArrowLeft','ArrowRight'].includes(event.key))return;event.preventDefault();
    if(frequencyPanPending||frequencyPan||frontendChanging)return;
    const view=frequencyViewport(state),step=event.shiftKey?10000:event.altKey?100:1000;
    const previous={context:frequencyPanContext(state),center:view.center,span:view.span,width:view.span/step,start_x:0};
    const plan=frequencyPanPlan(state,previous,event.key==='ArrowRight'?-1:1,connectionError,dirtyForms.has('tuningForm'));
    if(plan.error){toast(plan.error,'warning');return;}if(plan.changed)await commitFrequencyPan(previous,plan.center_hz);
  }
  function centeredVfoPlan(current,disconnected=false) {
    if(!current||disconnected)return {error:'アプリとの接続を確認してください。'};
    const center=current.input?.center_hz,fs=current.input?.sample_rate;
    if(!Number.isFinite(center)||center<=0||!Number.isFinite(fs)||fs<=0)return {error:'受信中心とサンプルレートを確認してください。'};
    const declared=current.input?.usable_bandwidth_hz,usable=Number.isFinite(declared)&&declared>0?Math.min(declared,fs*.9):fs*.9;
    const bandwidth=Math.floor(Math.min(200000,usable,center*2));
    if(!Number.isFinite(bandwidth)||bandwidth<1000||bandwidth>=fs)return {error:'受信中心に1 kHz以上のVFO帯域を確保できません。受信条件を確認してください。'};
    return {vfo:{frequency_hz:center,mode:'WFM',bandwidth_hz:bandwidth}};
  }
  async function addCenteredVfo() {
    let previousIds=new Set();
    try{
      const result=await action('add_vfo',()=>{
        const plan=centeredVfoPlan(state,connectionError);if(plan.error)throw new Error(plan.error);
        previousIds=new Set((state?.vfos||[]).map(v=>String(v.id)));return {vfo:plan.vfo};
      });
      if(!result)return null;
      const added=(result.state?.vfos||state?.vfos||[]).filter(v=>!previousIds.has(String(v.id))),created=added[added.length-1];
      if(created){const selected=await activateVfo(created.id,{focus:false});if(selected)toast(`VFO ${created.id}を受信中心へ追加しました${created.bandwidth_hz<200000?'。復調帯域幅は有効受信帯域に合わせて縮めています':''}`);}
      return result;
    }catch(error){toast(error.message||String(error),'error');return null;}
  }
  function beginVfoDrag(event) {
    if(!state||event.button!==0)return;const p=plotFrequency(event);if(p.x<p.s.left||p.x>p.s.left+p.s.width||p.y<p.s.top||p.y>p.s.top+p.s.height)return;
    canvasGestureTarget='empty';
    if(event.shiftKey){canvasGestureTarget='user';if(event.detail<2)addManualMarker(p.hz);return;}
    let userHit=null;for(const marker of userMarkers()){const x=p.s.left+(marker.frequency_hz-p.s.low)/p.s.span*p.s.width,distance=Math.abs(p.x-x);if(distance<8&&(!userHit||distance<userHit.distance))userHit={marker,distance};}
    if(userHit&&p.y>=p.s.top+17&&p.y<=p.s.top+32){canvasGestureTarget='user';event.preventDefault();selectUserMarker(userHit.marker.id);return;}
    if(selectedUserMarker()){
      canvasGestureTarget='user';event.preventDefault();
      if(userHit)selectUserMarker(userHit.marker.id);
      else userMarkerAction('user_marker_move',{frequency_hz:Math.round(p.hz)}).then(result=>{if(result)focusSpectrum();});
      return;
    }
    let best=null;
    (state.vfos||[]).forEach(v=>{const x=p.s.left+(v.frequency_hz-p.s.low)/p.s.span*p.s.width,half=v.bandwidth_hz/2/p.s.span*p.s.width;for(const [kind,at] of [['center',x],['low',x-half],['high',x+half]]){const distance=Math.abs(p.x-at);if(distance<(kind==='center'?13:7)&&(!best||distance<best.distance))best={v,kind,distance};}});
    if(!best){
      if(userHit){canvasGestureTarget='user';event.preventDefault();selectUserMarker(userHit.marker.id);}return;
    }
    canvasGestureTarget='vfo';
    const selection=(selectedUserMarker()||!best.v.selected)?activateVfo(best.v.id):Promise.resolve(true);
    drag={id:String(best.v.id),kind:best.kind,frequency_hz:best.v.frequency_hz,bandwidth_hz:best.v.bandwidth_hz,initial_hz:p.hz,initial_frequency:best.v.frequency_hz,initial_bandwidth:best.v.bandwidth_hz,selection};
    $('spectrumCanvas').setPointerCapture(event.pointerId);event.preventDefault();
    focusSpectrum();
  }
  function moveVfo(event) {
    const p=plotFrequency(event);hover={x:p.x,hz:p.hz};
    if(drag?.id){if(drag.kind==='center')drag.frequency_hz=Math.round(drag.initial_frequency+p.hz-drag.initial_hz);else drag.bandwidth_hz=Math.max(1000,Math.round(2*Math.abs(p.hz-drag.initial_frequency)));}
    $('cursorReadout').hidden=p.x<p.s.left||p.x>p.s.left+p.s.width;asText('cursorReadout',`${mhz(p.hz).toFixed(6)} MHz`);drawSpectrum();
  }
  async function finishVfo(event) {
    if(!drag?.id)return;const moved=drag;drag=null;try{$('spectrumCanvas').releasePointerCapture(event.pointerId);}catch{}
    if(!await moved.selection)return;
    await action('update_vfo',{id:wireId(moved.id),patch:{frequency_hz:moved.frequency_hz,bandwidth_hz:moved.bandwidth_hz}});drawSpectrum();
  }
  function vfoKey(event) {
    if(selectedUserMarker()&&['ArrowLeft','ArrowRight'].includes(event.key)){event.preventDefault();seekUserPeak(event.key==='ArrowLeft'?'left':'right');return;}
    const v=selectedVfo();if(!v)return;const step=event.shiftKey?10000:event.altKey?100:1000;
    const frequencyDelta=event.key==='ArrowLeft'?-step:event.key==='ArrowRight'?step:0,bandwidthDelta=['+','='].includes(event.key)?step:event.key==='-'?-step:0;
    if(frequencyDelta||bandwidthDelta){event.preventDefault();const selection=selectedUserMarker()?activateVfo(v.id):vfoSelectionPromise;
      action('update_vfo',()=>{const current=state?.vfos?.find(item=>String(item.id)===String(v.id)) || v;return {id:v.id,patch:frequencyDelta?{frequency_hz:current.frequency_hz+frequencyDelta}:{bandwidth_hz:Math.max(1000,current.bandwidth_hz+bandwidthDelta)}};},{guard:async()=>Boolean(await selection)&&state?.vfos?.some(item=>String(item.id)===String(v.id))});
    }
  }
  function vfoWheel(event) {
    if(!state)return;event.preventDefault();
    if(event.ctrlKey){const factor=event.deltaY>0?1.15:1/1.15;action('set_display',()=>({display:{span_hz:clamp(finite(state.display?.span_hz,state.input?.sample_rate)*factor,1000,state.input.sample_rate)}}));return;}
    const v=selectedVfo();if(!v)return;const step=event.shiftKey?10000:event.altKey?100:1000;
    const selection=selectedUserMarker()?activateVfo(v.id,{focus:false}):vfoSelectionPromise;
    const canvas=$('spectrumCanvas'),previous=canvas.dataset.pendingVfo===String(v.id)?finite(canvas.dataset.pendingFrequency,v.frequency_hz):v.frequency_hz;
    clearTimeout(tuningTimer);const pending=previous+(event.deltaY<0?step:-step);canvas.dataset.pendingFrequency=pending;canvas.dataset.pendingVfo=v.id;
    tuningTimer=setTimeout(()=>{delete canvas.dataset.pendingFrequency;delete canvas.dataset.pendingVfo;action('update_vfo',{id:v.id,patch:{frequency_hz:pending}},{guard:()=>selection});},120);
  }

  function applyLayout(values, saveLocal = true) {
    layout={...layout,...values};layout.spectrum_ratio=clamp(finite(layout.spectrum_ratio,.6),.52,.75);layout.vfo_width=clamp(finite(layout.vfo_width,340),280,460);layout.bottom_ratio=clamp(finite(layout.bottom_ratio,.5),.3,.7);
    const style=document.documentElement.style;style.setProperty('--spectrum-ratio',`${layout.spectrum_ratio*100}%`);style.setProperty('--vfo-width',`${layout.vfo_width}px`);style.setProperty('--bottom-ratio',`${layout.bottom_ratio*100}%`);
    if(saveLocal)try{localStorage.setItem('spectrum-studio-layout',JSON.stringify(layout));}catch{}
    requestAnimationFrame(drawAll);
  }
  function attachResize(id,kind) {
    $(id).addEventListener('pointerdown',event=>{event.preventDefault();const current={...layout};drag={layout:kind,x:event.clientX,y:event.clientY,current};$(id).setPointerCapture(event.pointerId);});
    $(id).addEventListener('pointermove',event=>{if(drag?.layout!==kind)return;const current=drag.current;
      if(kind==='spectrum')applyLayout({spectrum_ratio:current.spectrum_ratio+(event.clientY-drag.y)/$('analysisWorkspace').clientHeight});
      if(kind==='sidebar')applyLayout({vfo_width:current.vfo_width-(event.clientX-drag.x)});
      if(kind==='bottom')applyLayout({bottom_ratio:current.bottom_ratio+(event.clientX-drag.x)/$('lowerPanels').clientWidth});
    });
    const finish=event=>{if(drag?.layout!==kind)return;drag=null;try{$(id).releasePointerCapture(event.pointerId);}catch{}action('set_layout',{layout:{...layout}});};
    $(id).addEventListener('pointerup',finish);$(id).addEventListener('pointercancel',finish);
    $(id).addEventListener('keydown',event=>{if(!['ArrowLeft','ArrowRight','ArrowUp','ArrowDown'].includes(event.key))return;event.preventDefault();const plus=['ArrowRight','ArrowDown'].includes(event.key)?1:-1;if(kind==='spectrum')applyLayout({spectrum_ratio:layout.spectrum_ratio+plus*.025});if(kind==='sidebar')applyLayout({vfo_width:layout.vfo_width-plus*10});if(kind==='bottom')applyLayout({bottom_ratio:layout.bottom_ratio+plus*.025});action('set_layout',{layout:{...layout}});});
  }

  function flushAudio(){
    const sources=audioSources,stream=audioStreamGain,now=audioContext?.currentTime ?? 0,end=now+.004;
    audioEpoch++;audioSources=[];audioStreamGain=null;audioNextTime=0;audioRemoteEpoch=null;
    if(stream&&audioContext){try{stream.gain.cancelScheduledValues(now);stream.gain.setValueAtTime(stream.gain.value,now);stream.gain.linearRampToValueAtTime(0,end);}catch{}}
    for(const source of sources){try{source.stop(stream&&audioContext?end+.001:undefined);}catch{}}
    if(stream)setTimeout(()=>{try{stream.disconnect();}catch{}},20);
  }
  function renderAudioControl(){
    const current=`現在：音声${audioEnabled?'ON':'OFF'}`,next=audioEnabled?'音声をOFFにする':'音声をONにする',button=$('audioButton');
    asText('audioState',current);$('audioState')?.classList.toggle('active',audioEnabled);
    button.textContent=audioStarting?'音声を開始中…':next;button.disabled=audioStarting;button.classList.toggle('active',audioEnabled);
    button.setAttribute('aria-pressed',String(audioEnabled));button.setAttribute('aria-busy',String(audioStarting));
    button.setAttribute('aria-label',`${current}。${audioStarting?'音声を開始中':next}`);button.title=audioStarting?`${current}。ブラウザーの音声開始を待っています。`:`${current}。押すと${audioEnabled?'OFF':'ON'}になります。`;
  }
  function updateAudioCaption(){
    const v=selectedVfo();let caption='スピーカー出力はOFFです。選択中の1局だけを出力します。';
    if(audioEnabled)caption=connectionError?'接続待ちです。音声の再開を待っています。':!state?.running?'受信・再生の開始待ちです。':v?`VFO ${v.id} · ${v.mode} · ${mhz(v.frequency_hz).toFixed(4)} MHz${performance.now()-audioReceivedAt>2000?' · 音声待ち':''}`:'聴取するVFOを選択してください';
    asText('audioStatus',caption);
  }
  async function toggleAudio(){
    if(audioStarting||studioClosing||studioClosed)return;
    if(!audioEnabled){
      audioStarting=true;renderAudioControl();
      try{audioContext ||= new (window.AudioContext||window.webkitAudioContext)();if(!audioGain){audioGain=audioContext.createGain();audioGain.gain.value=num('volume');audioGain.connect(audioContext.destination);}await audioContext.resume();if(studioClosing||studioClosed)return;if(audioContext.state&&audioContext.state!=='running')throw new Error('ブラウザーの音声出力が停止しています。もう一度ONにしてください。');audioEnabled=true;audioSequence=0;}
      catch(error){audioEnabled=false;flushAudio();if(!studioClosed)toast(`音声を開始できません：${error.message}`,'error');}
      finally{audioStarting=false;}
    }else{audioEnabled=false;flushAudio();}
    if(!studioClosed){renderAudioControl();updateAudioCaption();}
  }
  function playPcm(data){
    if(!audioEnabled||!audioContext||!audioPacketMatchesState(data))return;
    const remoteEpoch=data.epoch;if(remoteEpoch!==undefined&&remoteEpoch!==null){if(audioRemoteEpoch!==null&&remoteEpoch!==audioRemoteEpoch)flushAudio();audioRemoteEpoch=remoteEpoch;}
    if(!data.data_b64)return;
    const bytes=decodedBytes(data.data_b64),n=Math.floor(bytes.length/2);if(!n)return;
    const sr=Number(data.sample_rate ?? 32000);if(!Number.isFinite(sr)||sr<=0)return;
    const now=audioContext.currentTime;if(audioNextTime-now>.6)flushAudio();
    const rebuffer=!audioStreamGain||audioNextTime<=now;
    if(!audioStreamGain){audioStreamGain=audioContext.createGain();audioStreamGain.gain.value=0;audioStreamGain.connect(audioGain);}
    if(rebuffer)audioNextTime=now+.025;
    if(rebuffer){audioStreamGain.gain.setValueAtTime(0,audioNextTime);audioStreamGain.gain.linearRampToValueAtTime(1,audioNextTime+.004);}
    const buffer=audioContext.createBuffer(1,n,sr),out=buffer.getChannelData(0),view=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength);
    for(let k=0;k<n;k++)out[k]=view.getInt16(k*2,true)/32768;
    const source=audioContext.createBufferSource();source.buffer=buffer;source.connect(audioStreamGain);source.start(audioNextTime);audioNextTime+=n/sr;audioSources.push(source);source.onended=()=>{audioSources=audioSources.filter(s=>s!==source);};audioReceivedAt=performance.now();updateAudioCaption();
  }

  function showSettings(tab='receiver'){activateTab(tab);if(!$('settingsDialog').open)$('settingsDialog').showModal();if(tab==='presets')refreshFiles();}
  function activateTab(tab){document.querySelectorAll('[data-tab]').forEach(el=>{const active=el.dataset.tab===tab;el.classList.toggle('active',active);el.setAttribute('aria-selected',String(active));});document.querySelectorAll('[data-tab-panel]').forEach(el=>el.hidden=el.dataset.tabPanel!==tab);}
  function presetLoadReason() {
    if(!state||connectionError)return 'アプリとの接続を確認してください。';
    if(inputLocked())return 'IQ保存・トリガ待機を終了してから読み込めます。';
    if(state.vfos?.some(v=>v.wav_recording))return 'WAV録音を終了してから読み込めます。';
    if(state.source==='replay')return 'IQ再生を停止し、「ライブへ戻る」で再生を閉じてください。';
    if(state.running)return '受信を停止してから読み込めます。現在の測定は継続しています。';
    if(actionInFlight)return '操作の反映後に読み込めます。';
    if(!$('presetSelect').value)return '保存した設定を選択してください。';
    return '';
  }
  function updatePresetLoadState() { const reason=presetLoadReason();$('presetLoadButton').disabled=Boolean(reason);$('presetLoadButton').title=reason;asText('presetLoadReason',reason || '受信・Arm・録音を停止した状態で設定を読み込みます。'); }
  function updateFftVisibility(){$('rbwField').hidden=$('fftMode').value==='manual';$('fftSizeField').hidden=$('fftMode').value!=='manual';}
  function renderConditions(){
    $('conditionList').innerHTML=extraConditions.map((c,k)=>`<div class="condition-row" data-condition="${k}"><select data-condition-field="metric" aria-label="条件 ${k+1} の判定"><option value="peak" ${c.metric!=='integrated'?'selected':''}>ピークレベル</option><option value="integrated" ${c.metric==='integrated'?'selected':''}>積算電力</option></select><input type="number" value="${finite(c.threshold_db,-55)}" step="1" data-condition-field="threshold_db" aria-label="条件 ${k+1} 閾値"><span>dBFS</span><button type="button" class="icon-button" data-condition-delete="${k}" aria-label="条件 ${k+1} を解除">×</button></div>`).join('');
  }
  async function addManualMarker(hz){const data=Number.isFinite(hz)?{frequency_hz:Math.round(hz)}:{};const result=await action('user_marker_add',data,{forceVfos:false});if(result)focusSpectrum();return result;}
  function downloadLink(label,path){const link=document.createElement('a');link.className='button quiet';link.textContent=label;link.href=`/api/download?path=${encodeURIComponent(path)}`;link.download=String(path).split(/[\\/]/).pop();return link;}
  async function refreshFiles(){
    try{fileData=await getJson('/api/files');const files=fileData.files || [];
      $('fileList').replaceChildren();if(!files.length){$('fileList').textContent='保存データはまだありません。';}
      for(const file of files){
        const row=document.createElement('div');row.className='file-item';
        const info=document.createElement('div');info.className='file-info';
        const title=document.createElement('strong');title.textContent=file.name || String(file.path).split(/[\\/]/).pop();
        const sub=document.createElement('span');sub.textContent=`${file.kind || 'データ'} · ${formatBytes(file.size ?? file.bytes)} · ${file.path}`;
        info.append(title,sub);row.append(info);
        if(file.kind==='iq'||/\.(sigmf-meta|cu8|iq|cf32)$/i.test(file.path || '')){
          const open=document.createElement('button');open.className='button';open.textContent='IQ再生';open.onclick=()=>openReplay(file.path);row.append(open);
          if(/\.sigmf-meta$/i.test(file.path)) {row.append(downloadLink('IQ',file.path.replace(/\.sigmf-meta$/i,'.sigmf-data')),downloadLink('条件',file.path));}
          else row.append(downloadLink('保存',file.path));
        } else row.append(downloadLink('保存',file.path));
        const copy=document.createElement('button');copy.className='button quiet';copy.textContent='パス';
        copy.onclick=()=>{navigator.clipboard?.writeText(file.path).then(()=>toast('パスをコピーしました')).catch(()=>toast(file.path));};
        row.append(copy);$('fileList').append(row);
      }
      const old=$('presetSelect').value;$('presetSelect').replaceChildren();const empty=document.createElement('option');empty.value='';empty.textContent='設定を選択';$('presetSelect').append(empty);for(const preset of fileData.presets || []){const op=document.createElement('option');op.value=typeof preset==='string'?preset:preset.name;op.textContent=op.value;$('presetSelect').append(op);}$('presetSelect').value=old;updatePresetLoadState();
    }catch(error){$('fileList').textContent=`保存データを取得できません：${error.message}`;}
  }
  async function openReplay(path){const r=await action('replay_open',{path});if(r){$('filesDialog').close();flushAudio();audioSequence=0;toast('保存IQを開きました。再生開始で解析します。');}}

  $('receiveButton').addEventListener('click',()=>action(state?.running?'stop':'start',state?.source==='replay'?{}:{source:$('sourceSelect').value}));
  $('sourceSelect').addEventListener('change',()=>{preferredSource=$('sourceSelect').value;});
  $('tuningForm').addEventListener('submit',async event=>{
    event.preventDefault();
    const center=num('centerMHz')*1e6;
    const analysis={fft_mode:$('fftMode').value,rbw_hz:num('rbwHz'),fft_size:num('fftSize')};
    const display={span_hz:num('spanMHz')*1e6,range_db:num('rangeDb')};
    if(Math.abs(num('refLevel')-finite(state?.display?.ref_level,state?.analysis?.ref_level ?? -20))>1e-9){display.ref_level=num('refLevel');display.ref_level_unit=levelUnit();}
    if (!$('centerMHz').disabled && Math.abs(center-finite(state?.input?.center_hz))>.01) { const r=await action('set_input',{input:{center_hz:center}});if(!r)return; }
    const current=state?.analysis || {};
    const changed=analysis.fft_mode!==(current.fft_mode || 'rbw') || (analysis.fft_mode==='manual'?analysis.fft_size!==current.fft_size:analysis.rbw_hz!==(current.rbw_hz ?? current.target_rbw_hz ?? current.enbw_hz ?? 750));
    if(changed){const r=await action('set_analysis',{analysis});if(!r)return;}
    await action('set_display',{display},{form:'tuningForm',message:'周波数・表示条件を適用しました'});
  });
  $('inputApply').addEventListener('click',()=>action('set_input',{input:{gain_db:num('gainDb'),agc:$('agc').checked,ppm:num('ppm')}},{message:'入力条件を適用しました'}));
  $('dcRejectEnabled').addEventListener('change',changeFrontend);
  $('fftMode').addEventListener('change',updateFftVisibility);
  for(const [key,id] of Object.entries({live:'traceLive',max:'traceMax',min:'traceMin',avg:'traceAvg'})) $(id).addEventListener('change',()=>action('set_display',{display:{traces:{[key]:$(id).checked}}}));
  $('traceResetButton').addEventListener('click',resetTraces);
  $('detector').addEventListener('change',()=>action('set_analysis',{analysis:{detector:$('detector').value}}));
  $('densityMetric').addEventListener('change',()=>action('set_analysis',{analysis:{density_metric:$('densityMetric').value}}));
  $('densityRangeForm').addEventListener('input',beginDensityRangeEdit);$('densityRangeForm').addEventListener('submit',submitDensityRange);
  $('waterfallRowSeconds').addEventListener('change',()=>action('set_analysis',{analysis:{waterfall_row_seconds:num('waterfallRowSeconds')}}));
  $('addVfoButton').addEventListener('click',addCenteredVfo);
  $('vfoList').addEventListener('click',vfoAction);
  $('vfoList').addEventListener('input',markVfoEdited);
  $('vfoList').addEventListener('focusin',event=>{if(event.target.matches('input,select')&&selectedUserMarker())selectUserMarker(null,{focus:false});});
  $('vfoList').addEventListener('change',event=>{markVfoEdited(event);if(event.target.dataset.vfoField==='mode'){const card=event.target.closest('.vfo-card');card.querySelector('[data-vfo-field=bandwidth]').value={AM:10,NFM:12.5,WFM:200}[event.target.value];}});
  $('vfoList').addEventListener('keydown',event=>{if(event.key==='Enter'&&event.target.matches('input')){event.preventDefault();event.target.closest('.vfo-card').querySelector(event.target.dataset.receptionField?'[data-vfo-action=reception-apply]':'[data-vfo-action=apply]').click();}});
  $('audioButton').addEventListener('click',toggleAudio);$('shutdownButton').addEventListener('click',shutdownStudio);$('volume').addEventListener('input',()=>{if(audioGain)audioGain.gain.setTargetAtTime(num('volume'),audioContext.currentTime,.02);});
  $('iqButton').addEventListener('click',()=>action(state?.recording?.iq_active?'iq_stop':'iq_start'));
  $('armButton').addEventListener('click',()=>action(state?.trigger?.armed?'disarm':'arm'));
  $('settingsButton').addEventListener('click',()=>showSettings());$('presetsButton').addEventListener('click',()=>showSettings('presets'));$('measurementButton').addEventListener('click',()=>showSettings('measurement'));$('triggerSettingsButton').addEventListener('click',()=>showSettings('trigger'));
  $('addUserMarkerButton').addEventListener('click',()=>addManualMarker());
  const chooseUserMarker=event=>{const button=event.target.closest('[data-user-marker-select]');if(button)selectUserMarker(button.dataset.userMarkerSelect);};
  $('userMarkerList').addEventListener('click',chooseUserMarker);$('markerTable').addEventListener('click',chooseUserMarker);
  const choosePeak=event=>{const button=event.target.closest('[data-peak-frequency]');if(button&&!button.disabled){event.preventDefault();event.stopPropagation();tunePeakFrequency(Number(button.dataset.peakFrequency),button.dataset.peakLabel || 'ピーク');}};
  $('markerReadout').addEventListener('click',choosePeak);$('markerTable').addEventListener('click',choosePeak);
  $('userMarkerTuneButton').addEventListener('click',()=>{const marker=selectedUserMarker();if(marker)tunePeakFrequency(marker.frequency_hz,`U${marker.id}`);});
  $('userMarkerDeselect').addEventListener('click',()=>selectUserMarker(null));
  $('userMarkerPeakLeft').addEventListener('click',()=>seekUserPeak('left'));$('userMarkerPeakRight').addEventListener('click',()=>seekUserPeak('right'));$('userMarkerPeakMax').addEventListener('click',()=>seekUserPeak('max'));
  $('userMarkerDelete').addEventListener('click',async()=>{const result=await userMarkerAction('user_marker_delete');if(result)focusSpectrum();});
  document.querySelectorAll('[data-tab]').forEach(el=>el.addEventListener('click',()=>{activateTab(el.dataset.tab);if(el.dataset.tab==='presets')refreshFiles();}));
  $('receiverForm').addEventListener('submit',async event=>{event.preventDefault();const input={sample_rate:num('sampleRate'),mode:$('inputMode').value},analysis={window:$('windowType').value,overlap:num('overlap'),average_seconds:num('averageSeconds')};const changed=input.sample_rate!==state?.input?.sample_rate || input.mode!==state?.input?.mode;const r=changed?await action('set_input',{input}):true;if(r)await action('set_analysis',{analysis},{form:'receiverForm',message:'入力・FFT条件を適用しました'});});
  $('persistenceForm').addEventListener('submit',event=>{event.preventDefault();action('set_analysis',{analysis:{persistence_mode:$('persistenceMode').value,persistence_unit:$('persistenceUnit').value,persistence_seconds:num('persistenceSeconds'),persistence_frames:num('persistenceFrames'),switch_policy:$('switchPolicy').value,stats_mode:$('statsMode').value}},{form:'persistenceForm',message:'密度・残像の設定を適用しました'});});
  $('measurementForm').addEventListener('submit',event=>{event.preventDefault();action('set_measurement',{measurement:{low_hz:num('measurementLowMHz')*1e6,high_hz:num('measurementHighMHz')*1e6,link_vfo:linkedVfo($('measurementLink').value),obw_percent:num('obwPercent'),marker_count:num('markerCount'),marker_mode:$('markerMode').value,marker_follow:$('markerFollow').checked,delta_reference:num('deltaReference')}},{form:'measurementForm',message:'測定条件を適用しました'});});
  document.querySelectorAll('[data-obw]').forEach(el=>el.addEventListener('click',()=>{$('obwPercent').value=el.dataset.obw;dirtyForms.add('measurementForm');}));
  $('addMarkerButton').addEventListener('click',()=>addManualMarker(num('manualMarkerMHz')*1e6));$('clearMarkersButton').addEventListener('click',async()=>{for(const marker of [...userMarkers()]){if(!await action('user_marker_delete',{id:marker.id},{forceVfos:false}))break;}});
  $('triggerForm').addEventListener('submit',event=>{event.preventDefault();const conditions=Array.from($('conditionList').querySelectorAll('.condition-row')).map(row=>({metric:row.querySelector('[data-condition-field=metric]').value,threshold_db:Number(row.querySelector('[data-condition-field=threshold_db]').value)}));action('set_trigger',{trigger:{mode:$('triggerMode').value,metric:$('triggerMetric').value,threshold_db:num('thresholdDb'),hysteresis_db:num('hysteresisDb'),pre_seconds:num('preSeconds'),post_seconds:num('postSeconds'),min_duration_s:num('minDurationSeconds'),rearm_s:num('rearmSeconds'),logic:$('triggerLogic').value,low_hz:num('triggerLowMHz')*1e6,high_hz:num('triggerHighMHz')*1e6,link_vfo:linkedVfo($('triggerLink').value),conditions}},{form:'triggerForm',message:'トリガ条件を適用しました'});});
  $('addConditionButton').addEventListener('click',()=>{extraConditions.push({metric:'peak',threshold_db:num('thresholdDb')});renderConditions();dirtyForms.add('triggerForm');});
  $('conditionList').addEventListener('click',event=>{const b=event.target.closest('[data-condition-delete]');if(b){extraConditions.splice(Number(b.dataset.conditionDelete),1);renderConditions();dirtyForms.add('triggerForm');}});
  $('conditionList').addEventListener('change',event=>{const row=event.target.closest('[data-condition]');if(row&&event.target.dataset.conditionField){extraConditions[Number(row.dataset.condition)][event.target.dataset.conditionField]=event.target.dataset.conditionField==='threshold_db'?Number(event.target.value):event.target.value;}});
  $('calibrationFile').addEventListener('change',async event=>{
    const file=event.target.files?.[0]; if(!file)return;
    try {
      if(file.size>120*1024)throw new Error('校正JSONは120 KiB以下のファイルを選択してください。');
      let calibration;try{calibration=JSON.parse(await file.text());}catch{throw new Error('校正JSONを読み取れません。JSON形式を確認してください。');}
      if(!calibration||Array.isArray(calibration)||typeof calibration!=='object')throw new Error('校正JSONの先頭はオブジェクトにしてください。');
      await action('calibration_load',{calibration},{message:'校正情報を読み込みました。適用条件と有効状態を確認してください'});
    }catch(error){toast(error.message,'error');}finally{event.target.value='';}
  });
  $('calibrationClear').addEventListener('click',()=>action('calibration_clear',{}, {message:'校正情報を解除し、相対レベルへ戻しました'}));
  $('levelUnit').addEventListener('change',async()=>{const result=await action('set_level_unit',{unit:$('levelUnit').value});if(!result)renderCalibration(true);});
  $('presetSaveForm').addEventListener('submit',async event=>{event.preventDefault();const r=await action('preset_save',{name:$('presetName').value.trim()},{message:'設定を保存しました'});if(r)refreshFiles();});
  $('presetSelect').addEventListener('change',updatePresetLoadState);
  $('presetLoadButton').addEventListener('click',async()=>{const reason=presetLoadReason();if(reason){updatePresetLoadState();return;}cancelPendingVfoTune();const r=await action('preset_load',{name:$('presetSelect').value},{forceState:true,message:'設定を読み込みました。受信・記録は手動で開始してください'});if(r)dirtyForms.clear();});
  $('resetLayoutButton').addEventListener('click',()=>{applyLayout({spectrum_ratio:.6,vfo_width:340,bottom_ratio:.5});action('set_layout',{layout});});
  $('filesButton').addEventListener('click',()=>{$('filesDialog').showModal();refreshFiles();});$('refreshFilesButton').addEventListener('click',refreshFiles);
  $('replayOpenForm').addEventListener('submit',event=>{event.preventDefault();openReplay($('replayPath').value.trim());});
  $('replaySeek').addEventListener('change',()=>{flushAudio();audioSequence=0;action('replay_seek',{seconds:num('replaySeek')});});
  $('replayCloseButton').addEventListener('click',()=>{flushAudio();action('replay_close');});
  $('noticeDismiss').addEventListener('click',()=>notice(''));
  for(const form of ['tuningForm','receiverForm','persistenceForm','measurementForm','triggerForm','densityRangeForm']) $(form).addEventListener('input',()=>dirtyForms.add(form));
  $('spectrumCanvas').addEventListener('pointerdown',event=>{if(!beginFrequencyPan(event))beginVfoDrag(event);});$('spectrumCanvas').addEventListener('pointermove',event=>{if(!moveFrequencyPan(event)){if(drag||!frequencyAxisHover(event))moveVfo(event);}});$('spectrumCanvas').addEventListener('pointerup',event=>{if(frequencyPan)finishFrequencyPan(event);else finishVfo(event);});$('spectrumCanvas').addEventListener('pointercancel',()=>{cancelFrequencyPan();drag=null;drawSpectrum();});
  $('spectrumCanvas').addEventListener('pointerleave',()=>{if(!drag){hover=null;$('cursorReadout').hidden=true;drawSpectrum();}});
  $('spectrumCanvas').addEventListener('keydown',vfoKey);$('spectrumCanvas').addEventListener('wheel',vfoWheel,{passive:false});
  $('spectrumCanvas').addEventListener('dblclick',event=>{if(canvasGestureTarget!=='empty'||selectedUserMarker()||event.shiftKey)return;const p=plotFrequency(event);action('add_vfo',{vfo:{frequency_hz:Math.round(p.hz),mode:'WFM',bandwidth_hz:200000}});});
  $('spanReadout').addEventListener('pointerdown',event=>beginFrequencyPan(event,true));$('spanReadout').addEventListener('pointermove',moveFrequencyPan);$('spanReadout').addEventListener('pointerup',finishFrequencyPan);$('spanReadout').addEventListener('pointercancel',cancelFrequencyPan);$('spanReadout').addEventListener('keydown',frequencyPanKey);
  attachResize('spectrumResizer','spectrum');attachResize('sidebarResizer','sidebar');attachResize('bottomResizer','bottom');
  new ResizeObserver(()=>requestAnimationFrame(drawAll)).observe($('workspace'));
  window.addEventListener('resize',()=>requestAnimationFrame(drawAll));
  try{const saved=JSON.parse(localStorage.getItem('spectrum-studio-layout'));if(saved)applyLayout(saved,false);}catch{}

  async function refreshState(force=false,fromAction=false){if(studioClosed)return;const epoch=stateEpoch;try{const next=await getJson('/api/state');if(!studioClosed&&(fromAction||(epoch===stateEpoch&&!actionInFlight)))applyState(next,force);}catch(error){if(studioClosed||studioClosing||epoch!==stateEpoch||actionInFlight&&!fromAction)return;if(!connectionError){notice('アプリとの接続が切れています。再接続を待っています。','error');connectionError=true;flushAudio();renderVfos();}renderFrontend();$('statusDot').className='status-dot error';asText('sessionStatus','接続待ち');asText('footerStatus',error.message);updatePresetLoadState();}}
  async function stateLoop(){if(studioClosed)return;await refreshState();if(!studioClosed)stateTimer=setTimeout(stateLoop,1000);}
  function requestFrame(){if(studioClosed)return;clearTimeout(frameTimer);if(framePending){frameRefreshRequested=true;return;}frameTimer=setTimeout(frameLoop,0);}
  async function frameLoop(){
    if(studioClosed)return;clearTimeout(frameTimer);if(framePending){frameRefreshRequested=true;return;}framePending=true;const epoch=frameEpoch;
    try{const f=await getJson('/api/frame');if(!studioClosed&&epoch===frameEpoch){if(f)acceptFrame(f);if(frameFailures>=3&&$('noticeText').textContent==='解析表示を更新できません。再接続を待っています。')notice('');frameFailures=0;}}
    catch{if(studioClosed||studioClosing||epoch!==frameEpoch)return;frameFailures++;if(frameFailures===3&&state?.running&&!connectionError&&!state?.device?.error)notice('解析表示を更新できません。再接続を待っています。','error');}
    finally{framePending=false;if(!studioClosed){const delay=frameRefreshRequested?0:!state?.running?1000:document.hidden?500:125;frameRefreshRequested=false;frameTimer=setTimeout(frameLoop,delay);}}
  }
  async function audioLoop(){if(studioClosed)return;if(audioEnabled&&state?.running&&!connectionError){const epoch=audioEpoch;try{const data=await getJson(`/api/audio?after=${audioSequence}`);if(!studioClosed&&epoch===audioEpoch&&audioEnabled&&state?.running&&!connectionError&&audioPacketMatchesState(data)){if(finite(data.sequence)<audioSequence)audioSequence=0;playPcm(data);if(data.data_b64)audioSequence=finite(data.sequence,audioSequence);}}catch{}}if(!studioClosed)audioTimer=setTimeout(audioLoop,90);}
  renderFrontend();renderAudioControl();updateFftVisibility();drawAll();stateLoop();frameLoop();audioLoop();
})();
