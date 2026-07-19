#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { chmod, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { isAbsolute, join, resolve } from 'node:path';

const SCHEMA = 'homeboy/cloudflare-worker-deploy-result/v1';
const MAX_GATE_BODY_BYTES = 65536;

async function main() {
  const contract = JSON.parse(await readFile(requiredArgument('--contract'), 'utf8'));
  validate(contract);
  const root = resolve(contract.repository.worktree);
  const secretValues = [];
  const redact = (value) => secretValues.reduce((text, secret) => String(text).split(String(secret)).join('[REDACTED]'), String(value));
  const childEnvironment = { ...process.env };
  for (const secret of contract.secrets) if (secret.env) delete childEnvironment[secret.env];
  const result = resultFor(contract);
  try {
    const resolvedSecrets = await preflight(contract, root, result, redact, childEnvironment);
    secretValues.push(...resolvedSecrets.map(({ value }) => value));
    const prior = await deployedState(contract, root, redact, childEnvironment);
    if (resolvedSecrets.length) await provisionSecrets(contract, root, resolvedSecrets, prior, result, redact, childEnvironment);
    await deployAndGate(contract, root, prior, 'deploy', result, redact, childEnvironment);
    if (contract.durability?.redeploy_same_revision) {
      const durabilityPrior = await deployedState(contract, root, redact, childEnvironment);
      if (contract.durability.rotate_secrets && resolvedSecrets.length) await provisionSecrets(contract, root, resolvedSecrets, durabilityPrior, result, redact, childEnvironment, 'durability_secret_rotation');
      await deployAndGate(contract, root, durabilityPrior, 'durability_redeploy', result, redact, childEnvironment);
    }
    result.status = 'succeeded';
  } catch (error) {
    result.status = 'failed';
    result.failure = { stage: error.stage || 'unknown', code: error.code || 'deployment_failed', message: redact(error.message) };
    result.remediation.push(...remediation(error.code));
  }
  await writeResult(root, contract, result);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  process.exitCode = result.status === 'succeeded' ? 0 : 1;
}

function resultFor(contract) {
  return { schema: SCHEMA, status: 'running', source_revision: contract.repository.revision, source_ref: contract.repository.ref || 'declared-worktree', config_ref: contract.wrangler.config_ref, target: { worker: contract.target.worker, account_id: contract.target.account_id }, stages: [], deployments: [], failure: null, remediation: [], artifact_policy: { secret_values: 'omitted', raw_provider_output: 'omitted', local_paths: 'omitted' } };
}

async function preflight(contract, root, result, redact, env) {
  const stage = start(result, 'preflight');
  try {
    const head = await command('git', ['rev-parse', 'HEAD'], root, redact, env, contract.timeout_ms);
    if (head.stdout.trim() !== contract.repository.revision) throw fail('source_revision_mismatch', 'Checked-out revision differs from the declared immutable revision.');
    const clean = await command('git', ['status', '--porcelain'], root, redact, env, contract.timeout_ms);
    if (clean.stdout.trim()) throw fail('source_not_clean', 'Repository worktree has uncommitted changes.');
    const config = await readConfig(resolvePath(root, contract.wrangler.config));
    if (config.account_id !== contract.target.account_id) throw fail('account_target_mismatch', 'Wrangler config account_id differs from the declared target.');
    for (const binding of contract.expected_bindings) if (!config.bindings.has(binding)) throw fail('binding_missing', `Wrangler config lacks expected binding ${binding}.`);
    const whoami = await command(contract.wrangler.binary, ['whoami', '--json', '--account', contract.target.account_id], root, redact, env, contract.timeout_ms);
    if (!authenticatedForAccount(whoami.stdout, contract.target.account_id)) throw fail('account_auth_failed', 'Wrangler authentication does not prove access to the declared account.');
    await command(contract.wrangler.binary, ['deploy', '--dry-run', '--config', resolvePath(root, contract.wrangler.config), '--name', contract.target.worker], root, redact, env, contract.timeout_ms);
    const secrets = [];
    for (const descriptor of contract.secrets) secrets.push({ name: descriptor.name, value: await readSecret(descriptor, root) });
    finish(stage, 'succeeded', { source_revision: head.stdout.trim(), config_ref: contract.wrangler.config_ref || contract.wrangler.config, expected_bindings: [...contract.expected_bindings], required_secret_names: contract.secrets.map(({ name }) => name) });
    return secrets;
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
    await runGates(contract.gates, id);
    finish(stage, 'succeeded', { prior_deployment: prior, deployed, gate_ids: contract.gates.map(({ id: gateId }) => gateId) });
  } catch (error) {
    error.stage ||= id;
    const rollbackResult = prior?.version_id ? await rollback(contract, root, prior, redact, env) : { status: 'not_available', reason: 'No single prior production version was recorded.' };
    const existing = result.deployments.find((deployment) => deployment.stage === id);
    if (existing) existing.rollback = rollbackResult;
    else result.deployments.push({ stage: id, source_revision: contract.repository.revision, prior_deployment: prior, deployed, rollback: rollbackResult });
    finish(stage, 'failed', { code: error.code || 'deployment_failed', rollback: rollbackResult }); throw error;
  }
}

async function deployedState(contract, root, redact, env) {
  const output = await command(contract.wrangler.binary, ['deployments', 'list', '--name', contract.target.worker, '--json'], root, redact, env, contract.timeout_ms);
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
  for (const gate of gates) {
    const response = await fetch(gate.url, { redirect: 'manual', signal: AbortSignal.timeout(gate.timeout_ms || 10000) });
    const body = await boundedBody(response, gate.max_body_bytes || MAX_GATE_BODY_BYTES);
    if (response.status !== (gate.expected_status || 200)) throw fail('http_gate_status_failed', `Gate ${gate.id} returned HTTP ${response.status}.`, stage);
    if (gate.expected_text && !body.includes(gate.expected_text)) throw fail('http_gate_text_failed', `Gate ${gate.id} did not contain expected text.`, stage);
  }
}

async function boundedBody(response, limit) { const reader = response.body?.getReader(); if (!reader) return ''; let size = 0; const chunks = []; while (true) { const { done, value } = await reader.read(); if (done) break; size += value.byteLength; if (size > limit) { await reader.cancel(); throw fail('http_gate_body_too_large', 'Gate response exceeded its declared body limit.'); } chunks.push(value); } return new TextDecoder().decode(Buffer.concat(chunks)); }
async function readSecret(secret, root) { if (secret.env) { if (!process.env[secret.env]) throw fail('secret_unavailable', `Required secret ${secret.name} is unavailable.`); return process.env[secret.env]; } const path = resolvePath(root, secret.file); if (!(await stat(path)).isFile()) throw fail('secret_unavailable', `Required secret ${secret.name} is unavailable.`); return readFile(path, 'utf8'); }
async function readConfig(path) { const raw = await readFile(path, 'utf8'); if (path.endsWith('.json') || path.endsWith('.jsonc')) { const parsed = JSON.parse(raw.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, '').replace(/,\s*([}\]])/g, '$1')); return { account_id: parsed.account_id, bindings: new Set(collectJsonBindings(parsed)) }; } return { account_id: matchToml(raw, 'account_id'), bindings: new Set([...raw.matchAll(/^\s*binding\s*=\s*["']([^"']+)["']/gm)].map((match) => match[1])) }; }
function collectJsonBindings(value) { if (!value || typeof value !== 'object') return []; const found = []; if (typeof value.binding === 'string') found.push(value.binding); for (const child of Object.values(value)) found.push(...collectJsonBindings(child)); return found; }
function matchToml(raw, key) { return raw.match(new RegExp(`^\\s*${key}\\s*=\\s*["']([^"']+)["']`, 'm'))?.[1]; }
function authenticatedForAccount(json, accountId) { try { return hasString(JSON.parse(json), accountId); } catch { return false; } }
function hasString(value, expected) { if (value === expected) return true; return value && typeof value === 'object' && Object.values(value).some((child) => hasString(child, expected)); }
async function command(executable, args, cwd, redact, env, timeoutMs = 120000, input) { const child = spawn(executable, args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] }); if (input) child.stdin.end(input); else child.stdin.end(); let stdout = ''; let stderr = ''; child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8'); child.stdout.on('data', (chunk) => { stdout += chunk; }); child.stderr.on('data', (chunk) => { stderr += chunk; }); let timedOut = false; const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); setTimeout(() => child.kill('SIGKILL'), 1000).unref(); }, timeoutMs); const code = await new Promise((resolveCode, reject) => { child.once('error', reject); child.once('close', resolveCode); }).finally(() => clearTimeout(timer)); if (timedOut) throw fail('command_timed_out', `Command exceeded ${timeoutMs}ms.`); if (code !== 0) throw fail('command_failed', `${executable} ${args.map(redact).join(' ')} failed: ${redact(stderr)}`); return { stdout: redact(stdout), stderr: redact(stderr) }; }
function start(result, id) { const stage = { id, status: 'running', evidence: null }; result.stages.push(stage); return stage; } function finish(stage, status, evidence) { stage.status = status; stage.evidence = evidence; } function fail(code, message, stage) { const error = new Error(message); error.code = code; error.stage = stage; return error; }
function remediation(code) { return ({ ambiguous_deployment_versions: ['Use a single-version production deployment or declare a weighted-version selection policy.'], source_not_clean: ['Commit or discard source changes before deployment.'], source_revision_mismatch: ['Check out the declared immutable revision before deployment.'], http_gate_status_failed: ['Correct the deployed route or expected status before retrying the immutable revision.'] }[code] || ['Inspect redacted stage evidence and correct the deployment contract.']); }
function resolvePath(root, value) { if (!value) throw fail('invalid_contract', 'Required path is missing.'); return isAbsolute(value) ? value : resolve(root, value); } function requiredArgument(name) { const index = process.argv.indexOf(name); if (index < 0 || !process.argv[index + 1]) throw new Error(`${name} is required`); return process.argv[index + 1]; }
function validate(contract) { if (contract.schema !== 'homeboy/cloudflare-worker-deploy-contract/v1') throw new Error('Unsupported deployment contract schema.'); for (const path of ['repository.worktree', 'repository.revision', 'wrangler.binary', 'wrangler.config', 'wrangler.config_ref', 'target.worker', 'target.account_id']) if (!path.split('.').reduce((value, key) => value?.[key], contract)) throw new Error(`Missing ${path}.`); contract.expected_bindings ||= []; contract.secrets ||= []; contract.gates ||= []; contract.timeout_ms ||= 120000; for (const secret of contract.secrets) if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(secret.name || '') || Boolean(secret.env) === Boolean(secret.file)) throw new Error('Each secret requires a safe environment-style name and exactly one environment or file descriptor.'); }
async function writeResult(root, contract, result) { if (contract.result_file) await writeFile(resolvePath(root, contract.result_file), `${JSON.stringify(result, null, 2)}\n`, { mode: 0o600 }); }
main().catch((error) => { process.stderr.write(`${JSON.stringify({ schema: SCHEMA, status: 'failed', code: error.code || 'invalid_contract', error: error.message })}\n`); process.exitCode = 1; });
