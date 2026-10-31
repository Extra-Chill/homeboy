import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-bench-runtime-diagnostics-'));

try {
  const componentPath = path.join(root, 'component');
  const artifactsPath = path.join(root, 'artifacts');
  const fakeWpCodebox = path.join(root, 'wp-codebox.cjs');
  await mkdir(componentPath, { recursive: true });
  await writeFile(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
if (args.includes('--version')) {
  process.stdout.write('wp-codebox 0.26.0\\n');
  process.exit(0);
}
if (args.join(' ') === 'runtime descriptor --json') {
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } }));
  process.exit(0);
}
if (args.slice(0, 3).join(' ') === 'recipe build bench') {
  const output = args[args.indexOf('--output') + 1];
  fs.writeFileSync(output, JSON.stringify({ schema: 'wp-codebox/recipe/v1' }));
  process.exit(0);
}
if (args[0] === 'recipe-run') {
  const artifacts = args[args.indexOf('--artifacts') + 1];
  fs.mkdirSync(artifacts, { recursive: true });
  fs.writeFileSync(path.join(artifacts, 'recipe-run-failure-diagnostics.json'), '{}\\n');
  process.stderr.write('runtime stderr credential=fixture-secret-value\\n');
  process.stdout.write(JSON.stringify({
    schema: 'wp-codebox/recipe-run/v1',
    success: false,
    error: {
      name: 'RecipeRuntimeCreateError',
      code: 'recipe-runtime-create-failed',
      message: 'Runtime creation failed before recipe workflow execution.',
      cause: {
        name: 'Error',
        code: 'MODULE_NOT_FOUND',
        message: "Cannot find module '@php-wasm/node-8-3/package.json' token=fixture-secret-value"
      }
    },
    phaseEvidence: [{ name: 'runtime_startup', status: 'failed' }]
  }) + '\\n');
  process.exit(1);
}
process.exit(2);
`);
  await chmod(fakeWpCodebox, 0o755);

  const adapter = path.join(import.meta.dirname, '..', 'scripts', 'bench', 'wp-codebox-bench-adapter.mjs');
  const result = spawnSync(process.execPath, [adapter], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...process.env,
      FIXTURE_TOKEN: 'fixture-secret-value',
      HOMEBOY_COMPONENT_ID: 'runtime-diagnostics-fixture',
      HOMEBOY_COMPONENT_PATH: componentPath,
      HOMEBOY_SETTINGS_JSON: '{}',
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifactsPath,
      HOMEBOY_WP_CODEBOX_BIN: fakeWpCodebox,
    },
  });

  assert.equal(result.status, 1);
  assert.match(result.stderr, /Failure classification: dependency_resolution/);
  assert.match(result.stderr, /@php-wasm\/node-8-3\/package\.json/);
  assert.doesNotMatch(result.stderr, /fixture-secret-value/);

  const persisted = JSON.parse(await readFile(path.join(artifactsPath, 'wp-codebox-bench-run-diagnostics.json'), 'utf8'));
  const diagnostic = persisted.diagnostics[0];
  assert.equal(diagnostic.phase, 'runtime_startup');
  assert.equal(diagnostic.failure_classification, 'dependency_resolution');
  assert.equal(diagnostic.root_cause.code, 'MODULE_NOT_FOUND');
  assert.match(diagnostic.root_cause.message, /token=\[REDACTED\]/);
  assert.equal(diagnostic.command_result.status, 1);
  assert.match(diagnostic.command_result.stderr, /credential=\[REDACTED\]/);
  assert.match(diagnostic.artifact_refs[0], /recipe-run-failure-diagnostics\.json$/);
  assert.ok(Buffer.byteLength(JSON.stringify(persisted)) < 8192);
  assert.doesNotMatch(JSON.stringify(persisted), /fixture-secret-value/);

  process.stdout.write('WP Codebox bench runtime diagnostics smoke passed.\n');
} finally {
  await rm(root, { recursive: true, force: true });
}
