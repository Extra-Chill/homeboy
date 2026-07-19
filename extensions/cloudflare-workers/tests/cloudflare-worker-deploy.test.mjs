#!/usr/bin/env node

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import { join, resolve } from 'node:path';

const extension = resolve(import.meta.dirname, '..');
const script = join(extension, 'scripts/cloudflare-worker-deploy.mjs');
const root = mkdtempSync(join(os.tmpdir(), 'homeboy-cloudflare-test-')); const worker = join(root, 'worker'); const bin = join(root, 'bin'); mkdirSync(worker); mkdirSync(bin);
writeFileSync(join(worker, 'wrangler.toml'), 'name = "example-worker"\naccount_id = "account-example"\n[[kv_namespaces]]\nbinding = "CACHE"\n');
writeFileSync(join(bin, 'wrangler'), `#!/usr/bin/env node
const fs=require('fs'); const a=process.argv.slice(2); fs.appendFileSync(process.env.CALLS,JSON.stringify(a)+'\\n');
if(a[0]==='whoami') process.stdout.write(JSON.stringify({accounts:[{id:'account-example'}]}));
else if(a[0]==='deployments'){const n=Number(fs.readFileSync(process.env.STATE,'utf8')); fs.writeFileSync(process.env.STATE,String(n+1)); const records=[{id:'deployment-old',created_on:'2026-01-01T00:00:00Z',versions:[{version_id:'version-old',percentage:100}]},{id:'deployment-new',created_on:'2026-01-02T00:00:00Z',versions:[{version_id:'version-new',percentage:100}]},{id:'deployment-durable',created_on:'2026-01-03T00:00:00Z',versions:[{version_id:'version-durable',percentage:100}]}]; const snapshots=[[records[0]],[records[0],records[1]],[records[0],records[1]],[...records]]; process.stdout.write(JSON.stringify(snapshots[Math.min(n,3)]));}
else if(a[0]==='secret'){let v='';process.stdin.on('data',c=>v+=c);process.stdin.on('end',()=>{if(v!==process.env.SECRET_VALUE||process.env.FAIL_SECRET==='1')process.exitCode=2;});}
`); chmodSync(join(bin, 'wrangler'), 0o755);
for (const command of [['init','-q'],['config','user.email','test@example.test'],['config','user.name','Test'],['add','.'],['commit','-qm','fixture']]) spawnSync('git', command, { cwd: worker });
const revision = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: worker, encoding: 'utf8' }).stdout.trim(); const calls = join(root, 'calls'); const state = join(root, 'state');
const base = { schema:'homeboy/cloudflare-worker-deploy-contract/v1', repository:{worktree:worker,revision,ref:'fixture'}, wrangler:{binary:join(bin,'wrangler'),config:'wrangler.toml',config_ref:'wrangler.toml'}, target:{worker:'example-worker',account_id:'account-example'}, expected_bindings:['CACHE'], secrets:[{name:'API_TOKEN',env:'TEST_SECRET'}], gates:[], durability:{redeploy_same_revision:true}, timeout_ms:5000 };

const success = await run(base); assert.equal(success.status, 0, `${success.stdout}\n${success.stderr}`);
if (process.env.HOMEBOY_EXTENSION_RUN === '1') { assert.match(readFileSync(calls,'utf8'), /"secret","put","API_TOKEN"/); rmSync(root,{recursive:true,force:true}); } else {
const successful = outputResult(success.stdout);
assert.equal(successful.status, 'succeeded'); assert.deepEqual(successful.stages.map(({id})=>id), ['preflight','secret_provisioning','deploy','durability_redeploy']); assert.equal(successful.deployments[0].prior_deployment.version_id, 'version-old'); assert.equal(successful.deployments[1].prior_deployment.version_id, 'version-new'); assert.equal(successful.deployments[0].deployed.version_id, 'version-new'); assert.equal(successful.deployments[1].deployed.version_id, 'version-durable'); assert.equal(success.stdout.includes(worker), false); assert.equal(success.stdout.includes('never-log-this-value'), false);
const successCalls=readFileSync(calls,'utf8'); assert.equal((successCalls.match(/"secret","put"/g)||[]).length,1); assert.equal(successCalls.includes('never-log-this-value'),false);

const server=await listen('not-ready'); try { const failed=await run({...base,gates:[{id:'health',url:`http://127.0.0.1:${server.port}/health`,expected_status:200,expected_text:'healthy'}],durability:{redeploy_same_revision:false}}); assert.equal(failed.status,1); const result=outputResult(failed.stdout); assert.equal(result.failure.code,'http_gate_text_failed', failed.stdout); assert.equal(result.deployments.length,1); assert.equal(result.deployments[0].deployed.version_id,'version-new'); assert.equal(result.deployments[0].rollback.restored_version_id,'version-old'); assert.match(readFileSync(calls,'utf8'),/"rollback","version-old"/); } finally { server.server.close(); }

const failedSecret=await run({...base,durability:{redeploy_same_revision:false}}, {FAIL_SECRET:'1'}); assert.equal(failedSecret.status,1); const secretResult=outputResult(failedSecret.stdout); assert.equal(secretResult.failure.stage,'secret_provisioning'); assert.equal(secretResult.deployments.at(-1).rollback.restored_version_id,'version-old'); assert.match(readFileSync(calls,'utf8'),/"rollback","version-old"/);

const invalidSecret=await run({...base,secrets:[{name:'../escape',env:'TEST_SECRET'}]}); assert.equal(invalidSecret.status,1); assert.equal(JSON.parse(invalidSecret.stderr).code,'invalid_contract');

rmSync(root,{recursive:true,force:true}); }
async function run(contract, environment={}) { writeFileSync(calls,'');writeFileSync(state,'0');const path=join(root,'contract.json');writeFileSync(path,JSON.stringify(contract));const throughHomeboy=process.env.HOMEBOY_EXTENSION_RUN==='1';const command=throughHomeboy?'homeboy':process.execPath;const args=throughHomeboy?['extension','run','cloudflare-workers','--','--contract',path]:[script,'--contract',path];return new Promise((resolveRun,reject)=>{const child=spawn(command,args,{env:{...process.env,CALLS:calls,STATE:state,TEST_SECRET:'never-log-this-value',SECRET_VALUE:'never-log-this-value',...environment},stdio:['ignore','pipe','pipe']});let stdout='';let stderr='';child.stdout.on('data',c=>stdout+=c);child.stderr.on('data',c=>stderr+=c);child.once('error',reject);child.once('close',status=>resolveRun({status,stdout,stderr}));}); }
function listen(body) { const server=http.createServer((_q,res)=>res.end(body));return new Promise((resolveListen,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',()=>resolveListen({server,port:server.address().port}));}); }
function outputResult(stdout) { const value=JSON.parse(stdout); return value.schema==='homeboy/command-result/v3' ? JSON.parse(value.data.output) : value; }
