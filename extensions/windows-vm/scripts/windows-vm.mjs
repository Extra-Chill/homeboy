#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { ProviderError, verifyRun } from './verify.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const encoded = text => Buffer.from(text, 'utf16le').toString('base64');
function exec(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', timeout: 120000, maxBuffer: 16 * 1024 * 1024, ...options });
  if (result.error || result.status !== 0) throw new ProviderError(result.error?.code === 'ETIMEDOUT' ? 'timeout' : 'transport_failed', `${command} failed`, { status: result.status, diagnostic: result.stdout?.trim() });
  return result.stdout.trim().replace(/^\uFEFF/, '');
}
function parse(args) {
  const options = { artifacts: [] };
  for (let i = 0; i < args.length; i++) {
    const key = args[i];
    if (['--reboot-cold', '--interactive'].includes(key)) options[key.slice(2)] = true;
    else if (['--as', '--command', '--script', '--artifact', '--timeout', '--config'].includes(key)) {
      if (args[i + 1] === undefined) throw new ProviderError('invalid_arguments', `Missing value for ${key}`);
      const value = args[++i];
      if (key === '--artifact') options.artifacts.push(value); else options[key.slice(2)] = value;
    } else throw new ProviderError('invalid_arguments', `Unknown option: ${key}`);
  }
  return options;
}
function config(options) {
  if (!options.config && !process.env.WINDOWS_VM_CONFIG) throw new ProviderError('invalid_config', 'Set WINDOWS_VM_CONFIG or --config');
  const c = JSON.parse(fs.readFileSync(options.config || process.env.WINDOWS_VM_CONFIG, 'utf8'));
  for (const key of ['directory', 'adminUser', 'standardUser', 'sshKey']) if (!c[key]) throw new ProviderError('invalid_config', `Missing ${key}`);
  c.directory = path.resolve(c.directory);
  c.sshKey = path.resolve(c.sshKey);
  c.name ??= 'homeboy-windows-vm'; c.image ??= 'dockurr/windows:latest';
  c.port ??= 2222; c.sshTimeout ??= 7200;
  c.adminPassword = process.env[c.adminPasswordEnv || 'WINDOWS_VM_ADMIN_PASSWORD'];
  c.standardPassword = process.env[c.standardPasswordEnv || 'WINDOWS_VM_STANDARD_PASSWORD'];
  if (!/^[a-zA-Z0-9_.-]+$/.test(c.name) || !/^[a-zA-Z][a-zA-Z0-9_-]{0,19}$/.test(c.standardUser) || c.standardUser === c.adminUser)
    throw new ProviderError('invalid_config', 'Invalid container or standard user name');
  for (const key of ['port', 'sshTimeout']) if (!Number.isInteger(c[key]) || c[key] < 1) throw new ProviderError('invalid_config', `Invalid ${key}`);
  return c;
}
function docker(c, ...args) { return exec('docker', args); }
function ssh(c, command, options = {}) {
  return exec('ssh', ['-p', String(c.port), '-i', c.sshKey, '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '-o', 'StrictHostKeyChecking=accept-new', '-o', `UserKnownHostsFile=${c.directory}/known_hosts`, `${c.adminUser}@127.0.0.1`, 'powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', encoded(command)], options);
}
function exists(c) { return spawnSync('docker', ['container', 'inspect', c.name], { stdio: 'ignore' }).status === 0; }
function stop(c) { if (exists(c)) exec('docker', ['stop', '-t', '120', c.name], { timeout: 180000 }); }
function state(c) { return JSON.parse(fs.readFileSync(`${c.directory}/identity.json`, 'utf8')); }
function writeJson(file, data) { fs.writeFileSync(file, JSON.stringify(data, null, 2) + '\n', { mode: 0o600 }); }
function snapshot(c) {
  stop(c);
  const identity = state(c);
  const disk = `${c.directory}/storage/data.img`;
  if (!fs.existsSync(disk)) throw new ProviderError('missing_disk', 'Expected dockurr storage/data.img');
  identity.imageHash = `sha256:${exec('sha256sum', [disk], { timeout: 600000 }).split(/\s/)[0]}`;
  writeJson(`${c.directory}/identity.json`, identity);
  const staging = `${c.directory}/base.new`;
  fs.rmSync(staging, { recursive: true, force: true }); fs.mkdirSync(staging);
  // Entire storage includes firmware variables, disks and backend metadata.
  exec('cp', ['-a', '--sparse=always', `${c.directory}/storage/.`, staging], { timeout: 600000 });
  fs.copyFileSync(`${c.directory}/identity.json`, `${staging}/identity.json`);
  fs.rmSync(`${c.directory}/base`, { recursive: true, force: true }); fs.renameSync(staging, `${c.directory}/base`);
}
function restore(c) {
  if (!fs.existsSync(`${c.directory}/base/identity.json`)) throw new ProviderError('missing_snapshot', 'Run prepare/snapshot first');
  stop(c);
  fs.rmSync(`${c.directory}/storage`, { recursive: true, force: true }); fs.mkdirSync(`${c.directory}/storage`);
  exec('cp', ['-a', '--sparse=always', `${c.directory}/base/.`, `${c.directory}/storage`], { timeout: 600000 });
  fs.copyFileSync(`${c.directory}/base/identity.json`, `${c.directory}/identity.json`);
}
async function wait(c) {
  const deadline = Date.now() + c.sshTimeout * 1000;
  while (Date.now() < deadline) {
    let status;
    try { status = ssh(c, `if (Test-Path C:\\OEM\\bootstrap.failed) { 'failed:' + (Get-Content -Raw C:\\OEM\\bootstrap.failed) } elseif (Test-Path C:\\OEM\\bootstrap.done) { 'ready' } else { 'bootstrapping' }`, { timeout: 10000 }).trim(); } catch { status = 'unreachable'; }
    if (status === 'ready') return;
    if (status.startsWith('failed:')) throw new ProviderError('bootstrap_failed', status.slice(7).trim());
    await sleep(status === 'unreachable' ? 2000 : 10000);
  }
  throw new ProviderError('ssh_timeout', `SSH not ready within ${c.sshTimeout}s`);
}
async function boot(c) { docker(c, 'start', c.name); await wait(c); }
function bootstrap(c) {
  if (!c.adminPassword || !c.standardPassword) throw new ProviderError('missing_secret', 'Admin and standard passwords are required on first prepare');
  fs.mkdirSync(`${c.directory}/oem`, { recursive: true, mode: 0o700 });
  fs.mkdirSync(`${c.directory}/storage`, { recursive: true });
  const publicKey = fs.readFileSync(c.publicKey || `${c.sshKey}.pub`, 'utf8').trim();
  writeJson(`${c.directory}/oem/bootstrap.json`, { publicKey, standardUser: c.standardUser, standardPassword: c.standardPassword });
  fs.copyFileSync(`${here}/bootstrap.ps1`, `${c.directory}/oem/bootstrap.ps1`);
  fs.writeFileSync(`${c.directory}/oem/install.bat`, '@echo off\r\npowershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:\\OEM\\bootstrap.ps1\r\n');
  const env = { ...process.env, USERNAME: c.adminUser, PASSWORD: c.adminPassword };
  exec('docker', ['create', '--name', c.name, '--restart=no', '--device', '/dev/kvm', '--device', '/dev/net/tun', '--cap-add', 'NET_ADMIN', '-e', 'USERNAME', '-e', 'PASSWORD', '-e', `VERSION=${c.version || '11'}`, '-e', `CPU_CORES=${c.cores || 4}`, '-e', `RAM_SIZE=${c.ram || '8G'}`, '-e', `DISK_SIZE=${c.disk || '64G'}`, '-v', `${c.directory}/storage:/storage`, '-v', `${c.directory}/oem:/oem`, '-p', `127.0.0.1:${c.port}:22`, '-p', `127.0.0.1:${c.rdpPort || 3389}:3389`, '-p', `127.0.0.1:${c.consolePort || 8006}:8006`, c.image], { env });
}
function expired(c) {
  if (!fs.existsSync(`${c.directory}/identity.json`)) return false;
  return Date.now() >= Date.parse(state(c).expiresAt);
}
async function prepare(c) {
  if (expired(c)) {
    stop(c); if (exists(c)) docker(c, 'rm', c.name);
    for (const item of ['base', 'storage', 'identity.json', 'known_hosts']) fs.rmSync(`${c.directory}/${item}`, { force: true, recursive: true });
  }
  if (fs.existsSync(`${c.directory}/base/identity.json`)) restore(c);
  if (!exists(c)) bootstrap(c);
  await boot(c);
  const guest = { build: ssh(c, '[Environment]::OSVersion.ToString()').trim() };
  const previous = fs.existsSync(`${c.directory}/identity.json`) ? state(c) : null;
  const image = docker(c, 'inspect', '--format', '{{.Image}}', c.name);
  const identity = { build: guest.build, backendImageHash: image, imageHash: previous?.imageHash, installedAt: previous?.installedAt || new Date().toISOString(), expiresAt: previous?.expiresAt || new Date(Date.now() + 89 * 86400000).toISOString() };
  writeJson(`${c.directory}/identity.json`, identity);
  if (!fs.existsSync(`${c.directory}/base/identity.json`)) { snapshot(c); await boot(c); }
  const runner = { id: c.name, kind: 'ssh', host: '127.0.0.1', port: c.port, user: c.adminUser, identity_file: c.sshKey, environment: { schema_version: 1, platform: 'windows', arch: 'x86_64', execution_kind: 'vm', os_version: identity.build } };
  writeJson(`${c.directory}/runner.json`, runner);
  return { imageIdentity: state(c), runner, runnerConfig: `${c.directory}/runner.json` };
}
async function run(c, o) {
  if (!['standard', 'admin'].includes(o.as) || Boolean(o.command) === Boolean(o.script)) throw new ProviderError('invalid_arguments', 'run requires --as standard|admin and exactly one of --command or --script');
  const timeout = Number(o.timeout || 300);
  if (!Number.isFinite(timeout) || timeout <= 0) throw new ProviderError('invalid_arguments', '--timeout must be positive seconds');
  const password = o.as === 'standard' ? c.standardPassword : c.adminPassword;
  if (!password && !o.interactive) throw new ProviderError('missing_secret', 'Task password is required');
  if (o['reboot-cold']) { docker(c, 'restart', c.name); await wait(c); }
  const id = randomUUID(), nonce = randomUUID(), startedAt = new Date().toISOString();
  const directory = path.resolve(c.artifactDirectory || `${c.directory}/artifacts`, id);
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const user = o.as === 'standard' ? c.standardUser : c.adminUser;
  const request = { id, nonce, startedAt, user, password, as: o.as, interactive: Boolean(o.interactive), timeout, artifacts: o.artifacts, script: o.script ? fs.readFileSync(o.script, 'utf8') : `${o.command}\n$commandSucceeded = $?\nif ($null -ne $LASTEXITCODE) { exit $LASTEXITCODE }\nif (!$commandSucceeded) { exit 1 }` };
  // Helper code travels in EncodedCommand; only request (including secret) uses stdin.
  // SSH outer deadline includes cleanup grace; guest enforces the workload deadline.
  try {
    const response = ssh(c, fs.readFileSync(`${here}/run.ps1`, 'utf8'), { input: JSON.stringify(request), timeout: (timeout + 120) * 1000 });
    let result;
    try { result = JSON.parse(response); } catch { throw new ProviderError('invalid_result', 'Guest helper did not return JSON'); }
    if (!result.ok) throw new ProviderError('guest_failed', result.error);
  } catch (error) {
    // Recover task cleanup even if the SSH session was interrupted. Never hide
    // inability to confirm cleanup behind the original transport error.
    try {
      ssh(c, `$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'homeboy-${id}' -ErrorAction SilentlyContinue; if ($t) { Stop-ScheduledTask -TaskName 'homeboy-${id}'; Unregister-ScheduledTask -TaskName 'homeboy-${id}' -Confirm:$false }; exit 0`, { timeout: 30000 });
    } catch { throw new ProviderError('cleanup_failed', `Cannot confirm task cleanup for run ${id}`, { runId: id, cause: error.message }); }
    // A nonzero guest status still carries the typed diagnostic on stdout.
    if (error.details?.diagnostic) {
      let guest;
      try { guest = JSON.parse(error.details.diagnostic); } catch { /* transport diagnostic */ }
      if (guest?.error) throw new ProviderError(guest.error.split(':')[0], guest.error);
    }
    throw error;
  }
  const artifacts = o.artifacts.map((remote, index) => ({ remote, local: `${directory}/artifacts/${index}` }));
  // scp's legacy protocol handles Windows drive paths; remote path is provider-generated.
  exec('scp', ['-O', '-r', '-P', String(c.port), '-i', c.sshKey, '-o', 'BatchMode=yes', '-o', `UserKnownHostsFile=${c.directory}/known_hosts`, `${c.adminUser}@127.0.0.1:C:/runs/${id}/*`, directory], { timeout: Math.max(120000, timeout * 1000) });
  // Windows PowerShell writes JSON with a UTF-8 BOM.
  const resultPath = `${directory}/result.json`;
  if (fs.existsSync(resultPath)) fs.writeFileSync(resultPath, fs.readFileSync(resultPath, 'utf8').replace(/^\uFEFF/, ''));
  let verified;
  try { verified = verifyRun({ directory, nonce, startedAt, artifacts }); }
  catch (error) {
    error.details = { ...error.details, runId: id, user, directory, artifacts, imageIdentity: state(c), durations: { totalMs: Date.now() - Date.parse(startedAt) } };
    throw error;
  }
  return { runId: id, user, ...verified, durations: { totalMs: Date.now() - Date.parse(startedAt) }, artifacts, imageIdentity: state(c) };
}
async function main() {
  const [verb, ...args] = process.argv.slice(2);
  const o = parse(args), c = config(o);
  if (!['prepare', 'reboot-cold', 'snapshot', 'reset', 'teardown', 'run'].includes(verb)) throw new ProviderError('invalid_arguments', 'Expected prepare, reboot-cold, snapshot, reset, teardown or run');
  fs.mkdirSync(c.directory, { recursive: true, mode: 0o700 });
  // A lease directory must never have concurrent lifecycle/run operations.
  const lock = `${c.directory}/operation.lock`;
  let fd;
  try { fd = fs.openSync(lock, 'wx', 0o600); } catch { throw new ProviderError('lease_busy', `Operation lock exists: ${lock}`); }
  try {
    let result;
    if (verb === 'prepare') result = await prepare(c);
    if (verb === 'reboot-cold') { docker(c, 'restart', c.name); await wait(c); result = { imageIdentity: state(c) }; }
    if (verb === 'snapshot') { snapshot(c); result = { snapshot: `${c.directory}/base`, stopped: true, imageIdentity: state(c) }; }
    if (verb === 'reset') { restore(c); result = { reset: true, stopped: true, imageIdentity: state(c) }; }
    if (verb === 'teardown') { stop(c); if (exists(c)) docker(c, 'rm', c.name); fs.rmSync(`${c.directory}/storage`, { recursive: true, force: true }); result = { teardown: 'removed', stopped: true, imageIdentity: fs.existsSync(`${c.directory}/identity.json`) ? state(c) : null }; }
    if (verb === 'run') result = await run(c, o);
    console.log(JSON.stringify({ ok: true, ...result }));
  } finally { fs.closeSync(fd); fs.unlinkSync(lock); }
}
main().catch(error => { console.log(JSON.stringify({ ok: false, error: { type: error.type || 'provider_failed', message: error.message, ...error.details } })); process.exitCode = 1; });
