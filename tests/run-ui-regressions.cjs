'use strict';
// Run only isolated source-level UI suites. Each run and every child result is
// new, with exclusive writes; this runner never invokes a saved verification script.
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const {spawn}=require('node:child_process');
const root=path.resolve(__dirname,'..');
const runId=`${new Date().toISOString().replace(/[:.]/g,'-')}-${process.pid}-${crypto.randomBytes(4).toString('hex')}`;
const output=path.join(root,'verification','ui-source-regressions',runId);
fs.mkdirSync(output,{recursive:true});
const sha=file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const appPath=path.join(root,'web/app.js'),appBefore=sha(appPath);
const suites=[
  ['static-boundaries','tests/ui-static-boundaries.cjs','json'],
  ['reception-boundaries','tests/ui-reception-boundaries.cjs','json'],
  ['frontend','tests/frontend-ui.test.cjs','node'],
  ['audio-state','tests/audio-ui-state.test.cjs','node'],
  ['shutdown','tests/app-shutdown.test.cjs','node'],
  ['frequency-pan','tests/frequency-axes-pan.test.cjs','node'],
  ['ssb-ui','tests/ssb-ui.test.cjs','node'],
  ['modulation-ui','tests/modulation-ui.test.cjs','node']
];
async function runSuite([name,file,kind]){
  const reportPath=path.join(output,`${name}.json`),started=new Date().toISOString(),sourceHash=sha(path.join(root,file));
  const result=await new Promise(resolve=>{
    const child=spawn(process.execPath,kind==='node'?['--test',file]:[file],{cwd:root,env:{...process.env,STUDIO_UI_REPORT_PATH:reportPath},windowsHide:true});
    let stdout='',stderr='';child.stdout.on('data',chunk=>stdout+=chunk);child.stderr.on('data',chunk=>stderr+=chunk);
    const timeout=setTimeout(()=>{child.kill();},30000);
    child.on('error',error=>{clearTimeout(timeout);resolve({exit_code:null,error:String(error),stdout,stderr});});
    child.on('close',(code,signal)=>{clearTimeout(timeout);resolve({exit_code:code,signal,stdout,stderr});});
  });
  const logPath=path.join(output,`${name}.log`);fs.writeFileSync(logPath,result.stdout+result.stderr,{flag:'wx'});
  let passed=null,total=null;
  if(kind==='json'&&fs.existsSync(reportPath)){const saved=JSON.parse(fs.readFileSync(reportPath,'utf8'));passed=saved.passed;total=saved.total;}
  if(kind==='node'){
    const count=result.stdout.match(/(?:ℹ|#) tests (\d+)/),pass=result.stdout.match(/(?:ℹ|#) pass (\d+)/);total=count?Number(count[1]):null;passed=pass?Number(pass[1]):null;
    fs.writeFileSync(reportPath,JSON.stringify({kind:'node:test isolated UI suite',started_at:started,source:file,source_sha256:sourceHash,app_js_sha256:appBefore,...result,passed,total},null,2),{flag:'wx'});
  }
  return {name,file,source_sha256:sourceHash,exit_code:result.exit_code,passed,total,report_path:reportPath,log_path:logPath};
}
(async()=>{
  const results=await Promise.all(suites.map(runSuite)),appAfter=sha(appPath);
  const report={kind:'isolated current-source UI regressions',mock_only:true,generated_at:new Date().toISOString(),app_js_sha256_before:appBefore,app_js_sha256_after:appAfter,
    source_unchanged_during_run:appBefore===appAfter,passed:results.reduce((sum,item)=>sum+(item.passed||0),0),total:results.reduce((sum,item)=>sum+(item.total||0),0),results};
  const reportPath=path.join(output,'summary.json');fs.writeFileSync(reportPath,JSON.stringify(report,null,2),{flag:'wx'});
  const ok=appBefore===appAfter&&results.every(result=>result.exit_code===0&&result.passed!==null&&result.passed===result.total);
  console.log(JSON.stringify({ok,passed:report.passed,total:report.total,report_path:reportPath,results:results.map(({name,exit_code,passed,total})=>({name,exit_code,passed,total}))},null,2));
  if(!ok)process.exitCode=1;
})().catch(error=>{console.error(error);process.exitCode=1;});
