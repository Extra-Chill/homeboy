#!/usr/bin/env node
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';

const requireFromHere = createRequire(import.meta.url);
const { preflightWpCodeboxCommand, preflightWpCodeboxRuntime, wpCodeboxCommand } = requireFromHere('../../lib/wp-codebox-runtime-selection.js');

const settings = json(process.env.HOMEBOY_SETTINGS_JSON, {});
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const slug = process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-bench-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const outputPath = path.join(directory, 'result.json');
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
  workloads: settings.wordpress_runtime_workloads,
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
  const result = run(['recipe-run', '--recipe', recipePath, '--artifacts', artifacts, '--json'], true);
  const envelope = json(result.stdout, {});
  if (!envelope.success || !envelope.benchResults) throw new Error('WP Codebox did not return a successful bench result envelope.');
  if (process.env.HOMEBOY_BENCH_RESULTS_FILE) {
    await mkdir(path.dirname(process.env.HOMEBOY_BENCH_RESULTS_FILE), { recursive: true });
    await writeFile(process.env.HOMEBOY_BENCH_RESULTS_FILE, `${JSON.stringify(envelope.benchResults, null, 2)}\n`);
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}

function run(args, capture = false) {
  const result = spawnSync(invocation.command, [...invocation.args, ...args], { cwd: componentPath, encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit' });
  if (result.error) throw result.error;
  if (capture && result.stdout) process.stdout.write(result.stdout);
  if (capture && result.stderr) process.stderr.write(result.stderr);
  if (result.status !== 0) process.exit(result.status ?? 1);
  return result;
}
function required(value, name) { if (!value) throw new Error(`${name} is required`); return value; }
function json(value, fallback) { try { return value ? JSON.parse(value) : fallback; } catch { return fallback; } }
function integer(value, fallback) { const number = Number(value); return Number.isInteger(number) && number > 0 ? number : fallback; }
function clean(value) { return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0))); }
