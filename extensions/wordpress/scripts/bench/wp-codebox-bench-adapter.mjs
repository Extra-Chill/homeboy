#!/usr/bin/env node
/**
 * External dependencies
 */
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
/**
 * Internal dependencies
 */
import { rigWorkloadInputs, selectedScenarioIds } from './wp-codebox-bench-selection.mjs';
import { boundedText } from '../lib/wp-codebox-timeout-diagnostics.mjs';

const requireFromHere = createRequire(import.meta.url);
const { preflightWpCodeboxCommand, preflightWpCodeboxRuntime, wpCodeboxCommand } = requireFromHere('../../lib/wp-codebox-runtime-selection.js');
const CHILD_OUTPUT_BYTES = 128 * 1024;

const settings = json(process.env.HOMEBOY_SETTINGS_JSON, {});
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const slug = process.env.HOMEBOY_COMPONENT_ID || process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-bench-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const scenarioIds = selectedScenarioIds(process.env.HOMEBOY_BENCH_SCENARIOS);
const rigInputs = rigWorkloadInputs(process.env.HOMEBOY_BENCH_EXTRA_WORKLOADS, scenarioIds, slug);
const options = clean({
  wordpressVersion: settings.wordpress_runtime_version,
  blueprint: settings.wordpress_runtime_blueprint,
  componentId: process.env.COMPONENT_ID,
  pluginSlug: slug,
  extra_plugins: [{ source: root, sourceSubpath: subpath, slug, activate: false }, ...(settings.wp_codebox_extra_plugins || [])],
  iterations: integer(process.env.HOMEBOY_BENCH_ITERATIONS, 3),
  warmupIterations: integer(process.env.HOMEBOY_BENCH_WARMUP_ITERATIONS, 1),
  env: settings.bench_env,
  wpConfigDefines: settings.wp_config_defines,
  mounts: [...(settings.wp_codebox_bench_mounts || []), ...rigInputs.mounts],
  workloads: [...(settings.wordpress_runtime_workloads || []), ...rigInputs.workloads],
  scenarioIds,
  prepareSteps: settings.wordpress_runtime_prepare_steps,
  postSteps: settings.wordpress_runtime_post_steps,
});
const runtime = preflightWpCodeboxRuntime({ env: process.env, settings });
if (!runtime.ready) {
  throw new Error(`WP Codebox runtime preflight failed: ${runtime.reason}; required >=${runtime.required_version}, observed ${runtime.selected.version || 'unavailable'} at ${runtime.selected.path || 'no executable'}. Run ${runtime.remediation}.`);
}
const invocation = wpCodeboxCommand(runtime.selected.path);
const commandPreflight = preflightWpCodeboxCommand([invocation.command, ...invocation.args], { env: process.env });
if (!commandPreflight.ready) {
  throw new Error(`WP Codebox command preflight failed: ${commandPreflight.reason}; required >=${commandPreflight.required_version}, observed ${commandPreflight.selected.version || 'unavailable'} at ${commandPreflight.selected.path || 'no executable'}. Run ${commandPreflight.remediation}.`);
}

try {
  await writeFile(optionsPath, `${JSON.stringify(options)}\n`);
  run(['recipe', 'build', 'bench', '--options', optionsPath, '--output', recipePath]);
  const result = run(['recipe-run', '--recipe', recipePath, '--artifacts', artifacts, '--json'], true, true);
  const parsed = parseEnvelope(result.stdout);
  const envelope = parsed.value;
  if (envelope.benchResults && process.env.HOMEBOY_BENCH_RESULTS_FILE) {
    await mkdir(path.dirname(process.env.HOMEBOY_BENCH_RESULTS_FILE), { recursive: true });
    await writeFile(process.env.HOMEBOY_BENCH_RESULTS_FILE, `${JSON.stringify(envelope.benchResults, null, 2)}\n`);
  }
  if (!parsed.valid || !envelope.success || !envelope.benchResults || result.status !== 0) {
    const diagnostic = benchRunFailureDiagnostic(envelope, result, parsed.valid);
    await mkdir(artifacts, { recursive: true });
    const diagnosticPath = path.join(artifacts, 'wp-codebox-bench-run-diagnostics.json');
    await writeFile(diagnosticPath, `${JSON.stringify({
      schema: 'homeboy/wp-codebox-bench-run-diagnostics/v1',
      diagnostics: [diagnostic],
    }, null, 2)}\n`);
    throw new Error([
      'WP Codebox wordpress.bench did not return a successful bench result envelope.',
      `Failure classification: ${diagnostic.failure_classification}.`,
      `Root cause: ${diagnostic.root_cause.message}`,
      `Evidence: artifact://${diagnosticPath}`,
    ].join(' '));
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}

function run(args, capture = false, allowFailure = false) {
  const result = spawnSync(invocation.command, [...invocation.args, ...args], { cwd: componentPath, encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (capture && result.stdout) {
    process.stdout.write(boundedText(result.stdout, CHILD_OUTPUT_BYTES));
  }
  if (capture && result.stderr) {
    process.stderr.write(boundedText(result.stderr, CHILD_OUTPUT_BYTES));
  }
  if (result.status !== 0 && !allowFailure) {
    process.exit(result.status ?? 1);
  }
  return result;
}

function parseEnvelope(value) {
  try {
    const parsed = JSON.parse(value);
    return { valid: parsed && typeof parsed === 'object' && !Array.isArray(parsed) && typeof parsed.success === 'boolean', value: parsed || {} };
  } catch {
    return { valid: false, value: {} };
  }
}

function benchRunFailureDiagnostic(envelope, result, validEnvelope) {
  const runtimeFailure = runtimeCreationFailure(envelope);
  const rootCause = deepestError(envelope.error) || deepestPhaseError(envelope.phaseEvidence) || {};
  let classification = boundedText(envelope.failure_classification, 128) || 'runtime_startup';
  if (!validEnvelope) {
    classification = 'malformed_output';
  } else if (runtimeFailure) {
    classification = runtimeFailureClassification(rootCause);
  }
  const rootMessage = boundedText(rootCause.message || envelope.message || result.stderr || result.stdout || 'WP Codebox returned no actionable failure message.', 1024);
  const artifactRefs = runtimeFailure
    ? [boundedText(`artifact://${path.join(artifacts, 'recipe-run-failure-diagnostics.json')}`, 1024)]
    : [];
  return {
    code: runtimeFailure ? 'wp-codebox-bench-runtime-create-failed' : 'wp-codebox-bench-run-failed',
    phase: runtimeFailure ? 'runtime_startup' : 'wordpress.bench',
    failure_classification: classification,
    message: runtimeFailure ? 'WP Codebox runtime creation failed before recipe workflow execution.' : 'WP Codebox benchmark execution failed.',
    root_cause: clean({
      name: boundedText(rootCause.name, 128),
      code: boundedText(rootCause.code, 128),
      message: rootMessage,
    }),
    command_result: clean({
      status: result.status,
      signal: result.signal,
      stdout: boundedText(result.stdout, 1024),
      stderr: boundedText(result.stderr, 1024),
    }),
    artifact_refs: artifactRefs,
    persisted_child_bench_results: Boolean(envelope.benchResults && process.env.HOMEBOY_BENCH_RESULTS_FILE),
  };
}

function runtimeCreationFailure(envelope) {
  if (errorChain(envelope.error).some((error) => error.code === 'recipe-runtime-create-failed')) {
    return true;
  }
  if (errorChain(envelope.error).some((error) => /runtime creation failed before recipe workflow execution/i.test(error.message || ''))) {
    return true;
  }
  return Array.isArray(envelope.phaseEvidence) && envelope.phaseEvidence.some((phase) => phase?.name === 'runtime_startup' && phase?.status === 'failed');
}

function runtimeFailureClassification(error) {
  const code = String(error.code || '');
  const message = String(error.message || '');
  const text = `${code} ${message}`;
  if (/MODULE_NOT_FOUND|ERR_MODULE_NOT_FOUND|cannot find (?:module|package)|could not resolve|failed to resolve|file dependency/i.test(text)) {
    return 'dependency_resolution';
  }
  if (/ETIMEDOUT|TIMEOUT|timed? out|deadline exceeded/i.test(text)) {
    return 'timeout';
  }
  if (/descriptor|manifest (?:is )?(?:invalid|missing)|invalid package\.json/i.test(text)) {
    return 'descriptor';
  }
  return 'runtime_startup';
}

function deepestError(error) {
  return errorChain(error).at(-1);
}

function errorChain(error) {
  const chain = [];
  let current = error;
  while (current && typeof current === 'object' && !Array.isArray(current)) {
    chain.push(current);
    current = current.cause;
  }
  return chain;
}

function deepestPhaseError(phases) {
  if (!Array.isArray(phases)) {
    return undefined;
  }
  const failed = phases.findLast((phase) => phase?.status === 'failed');
  return deepestError(failed?.error);
}

function required(value, name) { if (!value) { throw new Error(`${name} is required`); } return value; }
function json(value, fallback) { try { return value ? JSON.parse(value) : fallback; } catch { return fallback; } }
function integer(value, fallback) { const number = Number(value); return Number.isInteger(number) && number > 0 ? number : fallback; }
function clean(value) { return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0))); }
