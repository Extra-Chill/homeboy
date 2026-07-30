#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, mkdtemp, readFile, realpath, rm, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

const SCHEMA = 'homeboy/cloudflare-worker-deploy-result/v1';
const LAYERED_SCHEMA = 'homeboy/deployment-provider-payload/v1';
const LEGACY_SCHEMA = 'homeboy/cloudflare-worker-deploy-contract/v1';
const MAX_GATE_BODY_BYTES = 65536;
const MAX_SECRET_INPUT_BYTES = 65536;

async function main() {
  const dryRun = process.argv.includes('--dry-run');
  const contractPath = await realpath(resolve(requiredArgument('--contract')));
  const contract = normalizeContract(JSON.parse(await readFile(contractPath, 'utf8')));
  validate(contract);
  const secretValues = [];
  const redact = (value) => secretValues.reduce((text, secret) => String(text).split(String(secret)).join('[REDACTED]'), String(value));
  const childEnvironment = { ...process.env };
  for (const secret of [...contract.secrets, ...contract.secret_inputs]) if (secret.env) delete childEnvironment[secret.env];
  const root = await repositoryRoot(contract, contractPath, redact, childEnvironment);
  if (contract.repository.revision === 'HEAD') {
    contract.repository.revision = await repositoryHead(root, redact, childEnvironment, contract.timeout_ms);
  }
  const result = resultFor(contract, dryRun);
  try {
    const preflightResult = await preflight(contract, root, result, redact, childEnvironment, dryRun);
    if (dryRun) {
      result.status = 'validated';
    } else {
      const { workerSecrets, commandSecretInputs } = preflightResult;
      secretValues.push(...workerSecrets.map(({ value }) => value), ...commandSecretInputs.values());
      let prior = await deployedState(contract, root, redact, childEnvironment, contract.target.create_if_missing === true);
      const creating = !prior;
      if (contract.predeploy_commands.length) await runPredeployCommands(contract, root, result, redact, childEnvironment, commandSecretInputs);
      if (!prior) prior = await bootstrapWorker(contract, root, result, redact, childEnvironment);
      if (workerSecrets.length) await provisionSecrets(contract, root, workerSecrets, prior, result, redact, childEnvironment);
      if (creating && workerSecrets.length) prior = await deployedState(contract, root, redact, childEnvironment);
      await deployAndGate(contract, root, prior, 'deploy', result, redact, childEnvironment);
      if (contract.durability?.redeploy_same_revision) {
        const durabilityPrior = await deployedState(contract, root, redact, childEnvironment);
        if (contract.durability.rotate_secrets && workerSecrets.length) await provisionSecrets(contract, root, workerSecrets, durabilityPrior, result, redact, childEnvironment, 'durability_secret_rotation');
        await deployAndGate(contract, root, durabilityPrior, 'durability_redeploy', result, redact, childEnvironment);
      }
      result.status = 'succeeded';
    }
  } catch (error) {
    result.status = 'failed';
    result.failure = { stage: error.stage || 'unknown', code: error.code || 'deployment_failed', message: redact(error.message) };
    result.remediation.push(...remediation(error.code));
  }
  if (!dryRun) await writeResult(root, contract, result);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  process.exitCode = ['succeeded', 'validated'].includes(result.status) ? 0 : 1;
}

function normalizeContract(input) {
  if (input?.schema === LEGACY_SCHEMA) return input;
  if (input?.schema !== LAYERED_SCHEMA) throw new Error('Unsupported deployment contract schema.');
  assertKeys(input, ['schema', 'policy', 'target', 'source'], 'payload');
  if (!isObject(input.policy)) throw new Error('Layered deployment payload requires a policy object.');
  assertKeys(input.policy, ['value', 'reference'], 'policy envelope');
  const policy = input.policy?.value;
  const reference = input.policy?.reference;
  const target = input.target;
  const source = input.source;
  if (!isObject(policy) || !isObject(reference) || !isObject(target) || !isObject(source)) throw new Error('Layered deployment payload requires policy, target, and source objects.');
  assertKeys(reference, ['component', 'path', 'digest'], 'policy reference');
  assertKeys(source, ['component', 'revision'], 'source');
  assertKeys(policy, ['wrangler', 'expected_bindings', 'predeploy_commands', 'timeout_ms'], 'policy');
  assertKeys(target, ['target', 'secrets', 'secret_inputs', 'gates', 'durability'], 'target');
  if (!isObject(policy.wrangler)) throw new Error('Layered deployment policy requires a Wrangler object.');
  assertKeys(policy.wrangler, ['binary', 'config', 'config_ref'], 'policy Wrangler');
  if (policy.timeout_ms !== undefined && (!Number.isSafeInteger(policy.timeout_ms) || policy.timeout_ms < 1)) throw new Error('Layered deployment policy timeout_ms must be a positive safe integer.');
  if (!process.env.HOMEBOY_COMPONENT_PATH) throw new Error('Layered deployment payload requires HOMEBOY_COMPONENT_PATH.');
  if (!source.component || source.component !== process.env.HOMEBOY_COMPONENT_ID) throw new Error('Layered deployment source component does not match the execution context.');
  if (!/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/.test(source.revision || '')) throw new Error('Layered deployment source requires a full Git revision.');
  if (reference.component !== source.component || reference.path !== 'homeboy.json#/deployment_provider/policy' || !/^[0-9a-f]{64}$/.test(reference.digest || '')) throw new Error('Layered deployment policy reference is invalid.');
  if (createHash('sha256').update(canonicalJson(policy)).digest('hex') !== reference.digest) throw new Error('Layered deployment policy digest does not match the declared policy.');
  return {
    schema: LEGACY_SCHEMA,
    repository: { worktree: process.env.HOMEBOY_COMPONENT_PATH, revision: source.revision, ref: source.component },
    policy_reference: reference,
    wrangler: policy.wrangler,
    expected_bindings: policy.expected_bindings,
    predeploy_commands: policy.predeploy_commands,
    timeout_ms: policy.timeout_ms,
    target: target.target,
    secrets: target.secrets,
    secret_inputs: target.secret_inputs,
    gates: target.gates,
    durability: target.durability,
  };
}

function isObject(value) { return value !== null && typeof value === 'object' && !Array.isArray(value); }
function assertKeys(value, allowed, namespace) { for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new Error(`Unsupported layered ${namespace} field ${key}.`); }
function canonicalJson(value) { if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`; if (isObject(value)) return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`; return JSON.stringify(value); }

async function repositoryRoot(contract, contractPath, redact, env) {
  try {
    if (contract.repository.worktree === '.') {
      const repository = await command('git', ['rev-parse', '--show-toplevel'], dirname(contractPath), redact, env, contract.timeout_ms);
      return await realpath(repository.stdout.trim());
    }
    return await realpath(resolve(contract.repository.worktree));
  } catch {
    throw fail('invalid_contract', 'Declared repository worktree is unavailable.');
  }
}

async function repositoryHead(root, redact, env, timeoutMs) {
  try {
    const head = await command('git', ['rev-parse', 'HEAD'], root, redact, env, timeoutMs);
    return head.stdout.trim();
  } catch {
    throw fail('invalid_contract', 'Declared repository revision is unavailable.');
  }
}

async function bootstrapWorker(contract, root, result, redact, env) {
  const stage = start(result, 'bootstrap_create');
  try {
    await command(contract.wrangler.binary, ['deploy', '--config', resolvePath(root, contract.wrangler.config), '--name', contract.target.worker], root, redact, env, contract.timeout_ms);
    const deployed = await deployedState(contract, root, redact, env);
    result.deployments.push({ stage: 'bootstrap_create', source_revision: contract.repository.revision, prior_deployment: null, deployed });
    finish(stage, 'succeeded', { deployed });
    return deployed;
  } catch (error) {
    error.stage ||= 'bootstrap_create'; finish(stage, 'failed', { code: error.code || 'deployment_failed' }); throw error;
  }
}

async function runPredeployCommands(contract, root, result, redact, env, secretInputs) {
  const stage = start(result, 'predeploy_commands'); const commands = [];
  try {
    for (const declared of contract.predeploy_commands) {
      const startedAt = performance.now();
      const cwd = declared.cwd ? await commandCwd(root, declared.cwd) : root;
      const input = declared.stdin_secret_input ? secretInputs.get(declared.stdin_secret_input) : undefined;
      try { await command(declared.executable, declared.args, cwd, redact, env, declared.timeout_ms || contract.timeout_ms, input); }
      catch { throw fail('predeploy_command_failed', `Pre-deploy command ${declared.id} failed.`, 'predeploy_commands'); }
      commands.push({ id: declared.id, status: 'succeeded', elapsed_ms: Math.round(performance.now() - startedAt) });
    }
    const clean = await command('git', ['status', '--porcelain'], root, redact, env, contract.timeout_ms);
    if (clean.stdout.trim()) throw fail('source_changed_by_predeploy', 'Pre-deploy commands changed the declared immutable source.', 'predeploy_commands');
    finish(stage, 'succeeded', { commands });
  } catch (error) {
    finish(stage, 'failed', { code: error.code || 'predeploy_command_failed', commands });
    throw error;
  }
}

function resultFor(contract, dryRun) {
  return { schema: SCHEMA, status: 'running', mode: dryRun ? 'dry_run' : 'apply', source_revision: contract.repository.revision, source_ref: contract.repository.ref || 'declared-worktree', ...(contract.policy_reference ? { policy_reference: contract.policy_reference } : {}), config_ref: contract.wrangler.config_ref, target: { worker: contract.target.worker, account_id: contract.target.account_id }, stages: [], deployments: [], failure: null, remediation: [], artifact_policy: { secret_values: 'omitted', raw_provider_output: 'omitted', local_paths: 'omitted' } };
}

async function preflight(contract, root, result, redact, env, dryRun = false) {
  const stage = start(result, 'preflight');
  try {
    const head = await command('git', ['rev-parse', 'HEAD'], root, redact, env, contract.timeout_ms);
    if (head.stdout.trim() !== contract.repository.revision) throw fail('source_revision_mismatch', 'Checked-out revision differs from the declared immutable revision.');
    const clean = await command('git', ['status', '--porcelain'], root, redact, env, contract.timeout_ms);
    if (clean.stdout.trim()) throw fail('source_not_clean', 'Repository worktree has uncommitted changes.');
    const config = await readConfig(resolvePath(root, contract.wrangler.config));
    if (config.account_id && config.account_id !== contract.target.account_id) throw fail('account_target_mismatch', 'Wrangler config account_id differs from the declared target.');
    for (const binding of contract.expected_bindings) if (!config.bindings.has(binding)) throw fail('binding_missing', `Wrangler config lacks expected binding ${binding}.`);
    const whoami = await command(contract.wrangler.binary, ['whoami', '--json', '--account', contract.target.account_id], root, redact, env, contract.timeout_ms);
    if (!authenticatedForAccount(whoami.stdout, contract.target.account_id)) throw fail('account_auth_failed', 'Wrangler authentication does not prove access to the declared account.');
    await command(contract.wrangler.binary, ['deploy', '--dry-run', '--config', resolvePath(root, contract.wrangler.config), '--name', contract.target.worker], root, redact, env, contract.timeout_ms);
    let workerSecrets = [];
    let commandSecretInputs = new Map();
    if (dryRun) {
      for (const descriptor of contract.secrets) await validateSecretDescriptor(descriptor, root, 'secret');
      for (const descriptor of contract.secret_inputs) await validateSecretDescriptor(descriptor, root, 'secret input');
    } else {
      for (const descriptor of contract.secrets) workerSecrets.push({ name: descriptor.name, value: await readSecret(descriptor, root) });
      for (const descriptor of contract.secret_inputs) commandSecretInputs.set(descriptor.id, await readSecretInput(descriptor, root));
    }
    finish(stage, 'succeeded', { source_revision: head.stdout.trim(), config_ref: contract.wrangler.config_ref || contract.wrangler.config, expected_bindings: [...contract.expected_bindings], required_secret_names: contract.secrets.map(({ name }) => name), required_secret_input_ids: contract.secret_inputs.map(({ id }) => id), gate_ids: contract.gates.map(({ id }) => id), ...(dryRun ? { non_mutating: true, skipped: ['predeploy_commands', 'secret_provisioning', 'deployment', 'rollback', 'http_gates', 'result_file'] } : {}) });
    return { workerSecrets, commandSecretInputs };
  } catch (error) {
    error.stage ||= 'preflight'; finish(stage, 'failed', { code: error.code || 'preflight_failed', remediation: remediation(error.code) }); throw error;
  }
}

async function provisionSecrets(contract, root, secrets, prior, result, redact, env, id = 'secret_provisioning') {
  const stage = start(result, id); let temporaryDirectory;
  try {
    temporaryDirectory = await mkdtemp(join(os.tmpdir(), 'homeboy-cloudflare-worker-')); await chmod(temporaryDirectory, 0o700);
    for (const secret of secrets) {
      const material = join(temporaryDirectory, secret.name); await writeFile(material, secret.value, { mode: 0o600 }); await chmod(material, 0o600);
      await command(contract.wrangler.binary, ['secret', 'put', secret.name, '--name', contract.target.worker, '--config', resolvePath(root, contract.wrangler.config)], root, redact, env, contract.timeout_ms, await readFile(material));
    }
    finish(stage, 'succeeded', { required_secret_names: secrets.map(({ name }) => name), delivery: 'stdin', temporary_material: 'cleaned' });
  } catch (error) {
    error.stage ||= id;
    const rollbackResult = prior?.version_id ? await rollback(contract, root, prior, redact, env) : { status: 'not_available', reason: 'No single prior production version was recorded.' };
    result.deployments.push({ stage: id, source_revision: contract.repository.revision, prior_deployment: prior, deployed: null, rollback: rollbackResult });
    finish(stage, 'failed', { code: error.code || 'secret_provisioning_failed', rollback: rollbackResult, remediation: remediation(error.code) });
    throw error;
  }
  finally { if (temporaryDirectory) await rm(temporaryDirectory, { recursive: true, force: true }); }
}

async function deployAndGate(contract, root, prior, id, result, redact, env) {
  const stage = start(result, id);
  let deployed = null;
  try {
    await command(contract.wrangler.binary, ['deploy', '--config', resolvePath(root, contract.wrangler.config), '--name', contract.target.worker], root, redact, env, contract.timeout_ms);
    deployed = await deployedState(contract, root, redact, env);
    result.deployments.push({ stage: id, source_revision: contract.repository.revision, prior_deployment: prior, deployed });
    const gates = await runGates(contract.gates, id);
    finish(stage, 'succeeded', { prior_deployment: prior, deployed, gate_ids: contract.gates.map(({ id: gateId }) => gateId), gates });
  } catch (error) {
    error.stage ||= id;
    const rollbackResult = prior?.version_id ? await rollback(contract, root, prior, redact, env) : { status: 'not_available', reason: 'No single prior production version was recorded.' };
    const existing = result.deployments.find((deployment) => deployment.stage === id);
    if (existing) existing.rollback = rollbackResult;
    else result.deployments.push({ stage: id, source_revision: contract.repository.revision, prior_deployment: prior, deployed, rollback: rollbackResult });
    finish(stage, 'failed', { code: error.code || 'deployment_failed', rollback: rollbackResult, ...(error.gates ? { gates: error.gates } : {}) }); throw error;
  }
}

async function deployedState(contract, root, redact, env, allowMissing = false) {
  let output;
  try { output = await command(contract.wrangler.binary, ['deployments', 'list', '--name', contract.target.worker, '--json'], root, redact, env, contract.timeout_ms); }
  catch (error) { if (allowMissing && /does not exist|code:\s*10007/.test(error.message)) return null; throw error; }
  const deployments = JSON.parse(output.stdout); const records = Array.isArray(deployments) ? deployments : deployments.deployments; const current = latestDeployment(records);
  if (!current?.id) throw fail('deployment_identity_missing', 'Wrangler did not return a current deployment ID.');
  const versions = current.versions || (current.version_id ? [{ version_id: current.version_id, percentage: 100 }] : []);
  const active = versions.filter((version) => Number(version.percentage ?? version.traffic_percentage ?? 0) > 0);
  if (active.length !== 1) throw fail('ambiguous_deployment_versions', 'Current deployment does not select exactly one active version. Declare a selection policy before using weighted deployments.');
  const versionId = active[0].version_id || active[0].id;
  if (!versionId) throw fail('deployment_identity_missing', 'Current deployment selected version has no ID.');
  return { deployment_id: current.id, version_id: versionId };
}

function latestDeployment(records) {
  if (!Array.isArray(records) || !records.length) return null;
  return records.reduce((latest, record) => {
    const latestTime = Date.parse(latest?.created_on || '');
    const recordTime = Date.parse(record?.created_on || '');
    if (Number.isFinite(recordTime) && (!Number.isFinite(latestTime) || recordTime > latestTime)) return record;
    return latest;
  }, records.at(-1));
}

async function rollback(contract, root, prior, redact, env) {
  try { await command(contract.wrangler.binary, ['rollback', prior.version_id, '--name', contract.target.worker, '--config', resolvePath(root, contract.wrangler.config), '--yes'], root, redact, env, contract.timeout_ms); return { status: 'succeeded', restored_deployment_id: prior.deployment_id, restored_version_id: prior.version_id }; }
  catch (error) { return { status: 'failed', restored_version_id: prior.version_id, error: redact(error.message) }; }
}

async function runGates(gates, stage) {
  const evidence = [];
  for (const gate of gates) {
    try { evidence.push(await runGate(gate, stage)); }
    catch (error) { error.gates = [...evidence, { id: gate.id, attempts: error.gate_attempts || [] }]; throw error; }
  }
  return evidence;
}

async function runGate(gate, stage) {
  const attempts = [];
  const retry = gate.retry;
  const maximumAttempts = retry?.attempts || 1;
  try { for (let attempt = 1; attempt <= maximumAttempts; attempt += 1) {
    const startedAt = performance.now();
    try {
      const response = await fetch(gate.url, { redirect: 'manual', signal: AbortSignal.timeout(gate.timeout_ms || 10000) });
      const entry = { attempt, status: response.status, elapsed_ms: Math.round(performance.now() - startedAt) };
      attempts.push(entry);
      if (response.status !== (gate.expected_status || 200)) {
        if (attempt < maximumAttempts && retry.transient_statuses.includes(response.status)) { await delay(retry.retry_delay_ms); continue; }
        throw fail('http_gate_status_failed', `Gate ${gate.id} returned HTTP ${response.status}.`, stage);
      }
      const body = await boundedBody(response, gate.max_body_bytes || MAX_GATE_BODY_BYTES);
      if (gate.expected_text && !body.includes(gate.expected_text)) throw fail('http_gate_text_failed', `Gate ${gate.id} did not contain expected text.`, stage);
      return { id: gate.id, attempts };
    } catch (error) {
      if (error.code?.startsWith('http_gate_')) throw error;
      const timeout = error.name === 'TimeoutError' || error.name === 'AbortError';
      attempts.push({ attempt, status: timeout ? 'timeout' : 'network_error', elapsed_ms: Math.round(performance.now() - startedAt) });
      if (retry && attempt < maximumAttempts) { await delay(retry.retry_delay_ms); continue; }
      throw fail(timeout ? 'http_gate_timeout' : 'http_gate_network_failed', `Gate ${gate.id} ${timeout ? 'timed out' : 'could not be reached'}.`, stage);
    }
  } } catch (error) { error.gate_attempts = attempts; throw error; }
}

function delay(milliseconds) { return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds)); }

async function boundedBody(response, limit) { const reader = response.body?.getReader(); if (!reader) return ''; let size = 0; const chunks = []; while (true) { const { done, value } = await reader.read(); if (done) break; size += value.byteLength; if (size > limit) { await reader.cancel(); throw fail('http_gate_body_too_large', 'Gate response exceeded its declared body limit.'); } chunks.push(value); } return new TextDecoder().decode(Buffer.concat(chunks)); }
async function readSecret(secret, root) { if (secret.env) { if (!process.env[secret.env]) throw fail('secret_unavailable', `Required secret ${secret.name} is unavailable.`); return process.env[secret.env]; } const path = resolvePath(root, secret.file); if (!(await stat(path)).isFile()) throw fail('secret_unavailable', `Required secret ${secret.name} is unavailable.`); return readFile(path, 'utf8'); }
async function validateSecretDescriptor(secret, root, kind) { if (secret.env) { if (!Object.hasOwn(process.env, secret.env)) throw fail('secret_unavailable', `Required ${kind} ${secret.name || secret.id} is unavailable.`); return; } try { if (!(await stat(containedPath(root, await realpath(containedPath(root, secret.file))))).isFile()) throw new Error('not a file'); } catch { throw fail('secret_unavailable', `Required ${kind} ${secret.name || secret.id} is unavailable.`); } }
async function readSecretInput(secret, root) { let value; try { value = secret.env ? process.env[secret.env] : await readFile(containedPath(root, await realpath(containedPath(root, secret.file)))); } catch { throw fail('secret_unavailable', `Required secret input ${secret.id} is unavailable or outside its byte budget.`); } if (value === undefined || Buffer.byteLength(value) < 1 || Buffer.byteLength(value) > MAX_SECRET_INPUT_BYTES) throw fail('secret_unavailable', `Required secret input ${secret.id} is unavailable or outside its byte budget.`); return value; }
async function commandCwd(root, value) { try { return containedPath(root, await realpath(containedPath(root, value))); } catch { throw fail('invalid_contract', 'Pre-deploy command cwd must remain inside the repository.', 'predeploy_commands'); } }
async function readConfig(path) { const raw = await readFile(path, 'utf8'); if (path.endsWith('.json') || path.endsWith('.jsonc')) { const parsed = JSON.parse(stripJsonComments(raw).replace(/,\s*([}\]])/g, '$1')); return { account_id: parsed.account_id, bindings: new Set(collectJsonBindings(parsed)) }; } return { account_id: matchToml(raw, 'account_id'), bindings: new Set([...raw.matchAll(/^\s*binding\s*=\s*["']([^"']+)["']/gm)].map((match) => match[1])) }; }
function stripJsonComments(value) { let output=''; let string=false; let escaped=false; let line=false; let block=false; for(let index=0;index<value.length;index+=1){const character=value[index];const next=value[index+1];if(line){if(character==='\n'){line=false;output+=character;}continue;}if(block){if(character==='*'&&next==='/'){block=false;index+=1;}else if(character==='\n')output+=character;continue;}if(string){output+=character;if(escaped)escaped=false;else if(character==='\\')escaped=true;else if(character==='"')string=false;continue;}if(character==='"'){string=true;output+=character;}else if(character==='/'&&next==='/'){line=true;index+=1;}else if(character==='/'&&next==='*'){block=true;index+=1;}else output+=character;}return output; }
function collectJsonBindings(value) { if (!value || typeof value !== 'object') return []; const found = []; if (typeof value.binding === 'string') found.push(value.binding); if (Array.isArray(value.bindings)) for (const binding of value.bindings) if (typeof binding?.name === 'string') found.push(binding.name); for (const child of Object.values(value)) found.push(...collectJsonBindings(child)); return found; }
function matchToml(raw, key) { return raw.match(new RegExp(`^\\s*${key}\\s*=\\s*["']([^"']+)["']`, 'm'))?.[1]; }
function authenticatedForAccount(json, accountId) { try { return hasString(JSON.parse(json), accountId); } catch { return false; } }
function hasString(value, expected) { if (value === expected) return true; return value && typeof value === 'object' && Object.values(value).some((child) => hasString(child, expected)); }
async function command(executable, args, cwd, redact, env, timeoutMs = 120000, input) { const child = spawn(executable, args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] }); if (input) child.stdin.end(input); else child.stdin.end(); let stdout = ''; let stderr = ''; child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8'); child.stdout.on('data', (chunk) => { stdout += chunk; }); child.stderr.on('data', (chunk) => { stderr += chunk; }); let timedOut = false; const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); setTimeout(() => child.kill('SIGKILL'), 1000).unref(); }, timeoutMs); const code = await new Promise((resolveCode, reject) => { child.once('error', reject); child.once('close', resolveCode); }).finally(() => clearTimeout(timer)); if (timedOut) throw fail('command_timed_out', `Command exceeded ${timeoutMs}ms.`); if (code !== 0) throw fail('command_failed', `${executable} ${args.map(redact).join(' ')} failed: ${redact(stderr)}`); return { stdout: redact(stdout), stderr: redact(stderr) }; }
function start(result, id) { const stage = { id, status: 'running', evidence: null }; result.stages.push(stage); return stage; } function finish(stage, status, evidence) { stage.status = status; stage.evidence = evidence; } function fail(code, message, stage) { const error = new Error(message); error.code = code; error.stage = stage; return error; }
function remediation(code) { return ({ ambiguous_deployment_versions: ['Use a single-version production deployment or declare a weighted-version selection policy.'], source_not_clean: ['Commit or discard source changes before deployment.'], source_revision_mismatch: ['Check out the declared immutable revision before deployment.'], http_gate_status_failed: ['Correct the deployed route or expected status before retrying the immutable revision.'] }[code] || ['Inspect redacted stage evidence and correct the deployment contract.']); }
function resolvePath(root, value) { if (!value) throw fail('invalid_contract', 'Required path is missing.'); return isAbsolute(value) ? value : resolve(root, value); } function requiredArgument(name) { const index = process.argv.indexOf(name); if (index < 0 || !process.argv[index + 1]) throw new Error(`${name} is required`); return process.argv[index + 1]; }
function containedPath(root, value) { const path = resolve(root, value); const fromRoot = relative(root, path); if (fromRoot === '..' || fromRoot.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(fromRoot)) throw fail('invalid_contract', 'Pre-deploy command cwd must remain inside the repository.'); return path; }
function validate(contract) { if (contract.schema !== 'homeboy/cloudflare-worker-deploy-contract/v1') throw new Error('Unsupported deployment contract schema.'); for (const path of ['repository.worktree', 'repository.revision', 'wrangler.binary', 'wrangler.config', 'wrangler.config_ref', 'target.worker', 'target.account_id']) if (!path.split('.').reduce((value, key) => value?.[key], contract)) throw new Error(`Missing ${path}.`); contract.expected_bindings ||= []; contract.secrets ||= []; contract.secret_inputs ||= []; contract.gates ||= []; contract.predeploy_commands ||= []; contract.timeout_ms ||= 120000; if (!Array.isArray(contract.predeploy_commands) || contract.predeploy_commands.length > 16) throw new Error('predeploy_commands must be an array of at most 16 commands.'); if (!Array.isArray(contract.secret_inputs) || contract.secret_inputs.length > 16) throw new Error('secret_inputs must be an array of at most 16 inputs.'); const secretInputIds = new Set(); for (const secret of contract.secret_inputs) { if (!/^[a-z0-9]+(?:[-_.][a-z0-9]+)*$/.test(secret.id || '') || Boolean(secret.env) === Boolean(secret.file) || (secret.env !== undefined && !/^[A-Za-z_][A-Za-z0-9_]*$/.test(secret.env)) || (secret.file !== undefined && (typeof secret.file !== 'string' || !secret.file)) || secretInputIds.has(secret.id)) throw new Error('Each secret input requires a unique safe ID and exactly one environment or file descriptor.'); secretInputIds.add(secret.id); } for (const declared of contract.predeploy_commands) { const shell = basename(declared.executable || '').toLowerCase().replace(/\.exe$/, ''); if (!/^[a-z0-9]+(?:[-_.][a-z0-9]+)*$/.test(declared.id || '') || typeof declared.executable !== 'string' || !declared.executable || ['sh','bash','zsh','fish','cmd','powershell','pwsh'].includes(shell) || !Array.isArray(declared.args) || declared.args.length > 64 || declared.args.some((arg) => typeof arg !== 'string' || arg.length > 4096) || (declared.cwd !== undefined && (typeof declared.cwd !== 'string' || !declared.cwd)) || (declared.timeout_ms !== undefined && (!Number.isInteger(declared.timeout_ms) || declared.timeout_ms < 1)) || (declared.stdin_secret_input !== undefined && !secretInputIds.has(declared.stdin_secret_input))) throw new Error('Each pre-deploy command requires a safe ID, a non-shell executable, bounded string arguments, an optional repository-relative cwd, an optional declared stdin secret input, and an optional positive timeout_ms.'); }
if (contract.target.create_if_missing !== undefined && typeof contract.target.create_if_missing !== 'boolean') throw new Error('target.create_if_missing must be boolean when declared.');
for (const secret of contract.secrets) if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(secret.name || '') || Boolean(secret.env) === Boolean(secret.file)) throw new Error('Each secret requires a safe environment-style name and exactly one environment or file descriptor.'); for (const gate of contract.gates) if (gate.retry && (!Number.isInteger(gate.retry.attempts) || gate.retry.attempts < 2 || !Number.isInteger(gate.retry.retry_delay_ms) || gate.retry.retry_delay_ms < 0 || !Array.isArray(gate.retry.transient_statuses) || !gate.retry.transient_statuses.length || gate.retry.transient_statuses.some((status) => !Number.isInteger(status) || status < 100 || status > 599))) throw new Error('Each gate retry policy requires attempts of at least 2, a non-negative retry_delay_ms, and declared transient_statuses.'); }
async function writeResult(root, contract, result) { if (contract.result_file) await writeFile(resolvePath(root, contract.result_file), `${JSON.stringify(result, null, 2)}\n`, { mode: 0o600 }); }
main().catch((error) => { process.stderr.write(`${JSON.stringify({ schema: SCHEMA, status: 'failed', code: error.code || 'invalid_contract', error: error.message })}\n`); process.exitCode = 1; });
