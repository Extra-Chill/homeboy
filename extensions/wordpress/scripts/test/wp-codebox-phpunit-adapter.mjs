#!/usr/bin/env node
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const settings = json(process.env.HOMEBOY_SETTINGS_JSON, {});
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const slug = process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-phpunit-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const options = clean({
  wordpressVersion: settings.wordpress_runtime_version,
  pluginSlug: slug,
  extra_plugins: [{ source: root, sourceSubpath: subpath, slug, activate: false }],
  testRoot: settings.wp_codebox_phpunit_test_root,
  phpunitXml: settings.wp_codebox_phpunit_config,
  cwd: settings.wp_codebox_phpunit_cwd,
  phpunitArgs: process.argv.slice(2),
  env: settings.bench_env,
  wpConfigDefines: settings.wp_config_defines,
  bootstrapMode: settings.wp_codebox_phpunit_bootstrap_mode,
  projectBootstrap: settings.wp_codebox_phpunit_project_bootstrap,
  preloadFiles: settings.wp_codebox_phpunit_preload_files,
  mounts: settings.wp_codebox_phpunit_mounts,
});

try {
  await writeFile(optionsPath, `${JSON.stringify(options)}\n`);
  run(['recipe', 'build', 'phpunit', '--options', optionsPath, '--output', recipePath]);
  run(['recipe-run', '--recipe', recipePath, '--artifacts', artifacts, '--json']);
} finally {
  await rm(directory, { recursive: true, force: true });
}

function run(args) {
  const result = spawnSync(process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', args, { cwd: componentPath, encoding: 'utf8', stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
function required(value, name) { if (!value) throw new Error(`${name} is required`); return value; }
function json(value, fallback) { try { return value ? JSON.parse(value) : fallback; } catch { return fallback; } }
function clean(value) { return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0))); }
