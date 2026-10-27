"""Finite REPLAY-only acceptance checks. Never opens USB or controls an AFG.

Only a dedicated, initially idle v0.5.0 project is accepted. Evidence uses a new
directory on each run. Generated IQ is explicitly synthetic and stays private.
"""
from pathlib import Path
import argparse, hashlib, json, os, time, uuid, wave
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from datetime import datetime, timezone
import numpy as np

def sha(p): return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def canonical(p): return os.path.normcase(os.path.realpath(str(p).removeprefix('\\\\?\\')))
def write_new(p,v):
 with Path(p).open('x',encoding='utf-8') as f:json.dump(v,f,ensure_ascii=False,indent=2,allow_nan=False)
def require(v,msg):
 if not v:raise AssertionError(msg)

def fixture(project):
 fs=256000; n=fs*8; carrier=32000000; center=31950000
 name='synthetic_ssb_dual_sideband_validation'
 folder=project/'data'/'recordings';folder.mkdir(parents=True,exist_ok=True)
 raw=folder/(name+'.sigmf-data');meta=folder/(name+'.sigmf-meta')
 require(not raw.exists() and not meta.exists(),'synthetic fixture name already exists; use fresh runtime')
 # Distinct audio tones on opposite RF sides allow an end-to-end side selection
 # check. This deliberately mixed signal is unsuitable as ground-truth AMC input.
 t=np.arange(n,dtype=np.float64)/fs
 z=np.exp(2j*np.pi*(carrier-center)*t)*(.22*np.exp(2j*np.pi*1000*t)+.18*np.exp(-2j*np.pi*1800*t))
 q=np.empty(n*2,dtype=np.uint8);q[0::2]=np.rint(127.5+127.5*z.real).astype(np.uint8);q[1::2]=np.rint(127.5+127.5*z.imag).astype(np.uint8)
 with raw.open('xb') as f:f.write(q.tobytes())
 write_new(meta,{'global':{'core:datatype':'cu8','core:version':'1.2.6','core:sample_rate':fs,'core:num_channels':1,'core:dataset':raw.name,'core:description':'Digitally synthesized opposite-side tone fixture. No hardware or broadcast capture.','spectrum_studio:input':{'mode_actual':'tuner'},'spectrum_studio:recording':{'state':'complete','samples':n,'gaps':[]}},'captures':[{'core:sample_start':0,'core:frequency':center}],'annotations':[]})
 return meta,raw,carrier,fs

def main():
 ap=argparse.ArgumentParser(description=__doc__);ap.add_argument('--runtime',type=Path,required=True);ap.add_argument('--port',type=int,required=True);ap.add_argument('--output',type=Path,required=True);args=ap.parse_args()
 require(args.port!=8765,'refuse user active port')
 root=args.runtime.resolve();out=args.output/('replay-'+datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S')+'-'+uuid.uuid4().hex[:6]);out.mkdir(parents=True,exist_ok=False)
 base=f'http://127.0.0.1:{args.port}';checks=[];receipt={'success':False,'hardware_accessed':False,'runtime':str(root),'checks':checks,'observations':[]}
 def get(path):
  with urlopen(base+path,timeout=20) as r:return json.load(r)
 def action(action_name,**kw):
  req=Request(base+'/api/action',data=json.dumps({'action':action_name,**kw}).encode(),headers={'Content-Type':'application/json','Origin':base})
  with urlopen(req,timeout=20) as r:return json.load(r)
 def check(ok,label):require(ok,label);checks.append(label)
 try:
  h=get('/api/health');check(h['app']=='rtl-sdr-spectrum-studio' and h['version']=='0.5.0' and canonical(h['project'])==canonical(root),'exact dedicated app identity')
  s=get('/api/state');check(not s['running'] and not s['trigger']['armed'] and not s['recording']['iq_active'] and not any(v['wav_recording'] for v in s['vfos']),'initially idle no active recording')
  meta,raw,f0,fs=fixture(root);original=sha(raw);write_new(out/'fixture.json',{'meta':str(meta),'raw_sha256':original,'sample_rate':fs,'synthetic':True})
  action('replay_open',path=str(meta));action('set_frontend',frontend={'dc_reject_enabled':False})
  vid=get('/api/state')['selected_vfo_id']
  for mode,wanted,rejected in [('USB',1000,1800),('LSB',1800,1000)]:
   action('replay_seek',seconds=0)
   action('update_vfo',id=vid,patch={'mode':mode,'frequency_hz':f0,'bandwidth_hz':2700,'enabled':True,'reception':{'afc_enabled':False,'wav_audio':'demod','audio_gain_db':0}})
   before=get('/api/state');v=next(v for v in before['vfos'] if v['id']==vid)
   edges=(f0+300,f0+3000) if mode=='USB' else (f0-3000,f0-300)
   check((v['rf_low_hz'],v['rf_high_hz'])==edges,mode+' derived one sided RF bounds')
   try:action('update_vfo',id=vid,patch={'reception':{'afc_enabled':True}})
   except HTTPError as e:check(e.code==400,mode+' rejects unsupported AFC')
   else:raise AssertionError('SSB AFC unexpectedly accepted')
   action('start',source='replay');action('wav_start',id=vid)
   deadline=time.monotonic()+15
   while time.monotonic()<deadline:
    s=get('/api/state')
    if s['replay']['position_seconds']>=2.2:break
    time.sleep(.15)
   check(s['running'] and s['source']=='replay',mode+' real replay processing')
   action('wav_stop',id=vid);s=get('/api/state');v=next(v for v in s['vfos'] if v['id']==vid);summary=v['wav_summary'];action('stop')
   wav_path=Path(str(summary['path']).removeprefix('\\\\?\\'))
   check(canonical(wav_path).startswith(canonical(root)+os.sep),mode+' WAV inside dedicated project')
   with wave.open(str(wav_path),'rb') as w:
    rate=w.getframerate();sw=w.getsampwidth();channels=w.getnchannels();frames=w.getnframes();b=w.readframes(frames)
   check(rate==32000 and sw==2 and channels==1 and frames>rate,mode+' finalized 32k mono PCM16 WAV')
   samples=np.frombuffer(b,dtype='<i2');a=samples[int(.4*rate):-int(.1*rate)].astype(float);a*=np.hanning(len(a));sp=np.abs(np.fft.rfft(a));freq=np.fft.rfftfreq(len(a),1/rate)
   def tone(hz):return float(np.max(sp[np.abs(freq-hz)<5]))
   peak=float(freq[np.argmax(sp)]);ratio=20*np.log10(max(tone(rejected),1e-20)/max(tone(wanted),1e-20))
   check(abs(peak-wanted)<2,mode+' expected audio tone frequency')
   check(ratio<-35,mode+' opposite side rejection below -35dB synthetic')
   check(not np.any((samples==-32768)|(samples==32767)),mode+' WAV no integer rail samples')
   check(sha(raw)==original,mode+' original IQ unmodified')
   receipt['observations'].append({'mode':mode,'audio_peak_hz':peak,'opposite_tone_db':float(ratio),'frames':frames,'wav_sha256':sha(wav_path),'summary':summary})
  action('replay_close');check(not get('/api/state')['running'],'final idle state')
  receipt['success']=True
 except Exception as e:
  receipt['error']=repr(e)
  try:action('stop')
  except Exception:pass
 finally:write_new(out/'result.json',receipt)
 print(json.dumps({'success':receipt['success'],'checks':len(checks),'result':str(out/'result.json'),'error':receipt.get('error')},ensure_ascii=False))
 if not receipt['success']:raise SystemExit(1)
if __name__=='__main__':main()
