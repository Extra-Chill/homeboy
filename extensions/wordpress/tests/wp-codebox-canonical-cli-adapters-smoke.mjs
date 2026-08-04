/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-canonical-adapter-'));
const component = path.join(root, 'canonical-plugin');
const dependency = path.join(root, 'db-touching-dependency');
const observed = path.join(root, 'observed.jsonl');
const cli = path.join(root, 'wp-codebox');
const results = path.join(root, 'results', 'bench.json');
const testResults = path.join(root, 'results', 'test.json');
const writeResults = path.join(root, 'write-test-results.sh');
await mkdir(component, { recursive: true });
await mkdir(dependency, { recursive: true });
await writeFile(writeResults, `homeboy_write_test_results() { printf '{"total":%s,"passed":%s,"failed":%s,"skipped":%s}\n' "$1" "$2" "$3" "$4" > "$HOMEBOY_TEST_RESULTS_FILE"; }
`);
await writeFile(cli, `#!/usr/bin/env node
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
await appendFile(process.env.OBSERVED, JSON.stringify(args) + '\\n');
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = JSON.parse(await readFile(args[args.indexOf('--options') + 1], 'utf8'));
  await appendFile(process.env.OBSERVED, JSON.stringify({ options }) + '\\n');
  await writeFile(args[args.indexOf('--output') + 1], JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1' }));
  process.exit(0);
}
if (args[0] === 'recipe-run') {
  const artifacts = args[args.indexOf('--artifacts') + 1];
  await mkdir(artifacts + '/runtime-fixture/files', { recursive: true });
  await writeFile(artifacts + '/latest-runtime.json', JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
  await writeFile(artifacts + '/runtime-fixture/files/test-results.json', JSON.stringify({ schema: 'wp-codebox/test-results/v1', status: 'passed', summary: { total: 1, passed: 1, failed: 0, skipped: 0 } }));
  process.stdout.write(JSON.stringify({ success: true, benchResults: { schema: 'homeboy/bench-results/v1', scenarios: [] } }));
}
`);
await chmod(cli, 0o755);

try {
  const base = {
    HOMEBOY_COMPONENT_PATH: component,
    COMPONENT_ID: 'canonical-plugin',
    HOMEBOY_WP_CODEBOX_BIN: cli,
    OBSERVED: observed,
    HOMEBOY_SETTINGS_JSON: JSON.stringify({
      wp_codebox_source_root: '/workspace/monorepo',
      wp_codebox_source_subpath: 'plugins/canonical-plugin',
      validation_dependencies: [component, dependency, dependency],
      wordpress_runtime_workloads: [{ id: 'canonical-workload', run: [] }],
      wordpress_runtime_blueprint: { steps: [] },
      wordpress_runtime_prepare_steps: [{ command: 'wordpress.wp-cli', args: ['command=option get home'] }],
      wordpress_runtime_post_steps: [{ command: 'wordpress.browser-probe', args: ['url=/'] }],
    }),
  };
  const extension = path.resolve(import.meta.dirname, '..');
  const bench = spawnSync(path.join(extension, 'scripts/bench/bench-runner-wp-codebox.sh'), [], { env: { ...process.env, ...base, HOMEBOY_BENCH_RESULTS_FILE: results }, encoding: 'utf8' });
  assert.equal(bench.status, 0, bench.stderr);
  const phpunit = spawnSync(path.join(extension, 'scripts/test/test-runner-wp-codebox.sh'), [], { env: { ...process.env, ...base, HOMEBOY_TEST_RESULTS_FILE: testResults, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: writeResults }, encoding: 'utf8' });
  assert.equal(phpunit.status, 0, phpunit.stderr);
  assert.match(phpunit.stdout, /WP Codebox test run complete\./);
  const entries = (await readFile(observed, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.deepEqual(entries.filter(Array.isArray).map((args) => args.slice(0, 2)), [
    ['recipe', 'build'], ['recipe-run', '--recipe'], ['recipe', 'build'], ['recipe-run', '--recipe'],
  ]);
  const options = entries.filter((entry) => entry.options).map((entry) => entry.options);
  assert.deepEqual(options[0].extra_plugins, [
    { source: '/workspace/monorepo', sourceSubpath: 'plugins/canonical-plugin', slug: 'canonical-plugin', activate: false },
  ]);
  // The PHPUnit recipe activates declared validation dependencies first and
  // never leaves the plugin under review inactive: an inactive target is
  // excluded from WP Codebox's activation phase and Composer autoloader
  // preloading, which produces a sandbox where nothing can execute.
  assert.deepEqual(options[1].extra_plugins, [
    { source: dependency, slug: 'db-touching-dependency', activate: true },
    { source: '/workspace/monorepo', sourceSubpath: 'plugins/canonical-plugin', slug: 'canonical-plugin', activate: true },
  ]);
  assert.deepEqual(options[1].dependencyMounts, [
    '/wordpress/wp-content/plugins/canonical-plugin',
    '/wordpress/wp-content/plugins/db-touching-dependency',
  ]);
  assert.deepEqual(options[1].mounts, [{ source: path.join(extension, 'vendor'), target: '/wp-codebox-vendor', mode: 'readonly' }]);
  assert.equal(options[1].multisite, false, 'phpunit recipe defaults to single-site when nothing requests multisite');
  assert.equal(options[0].workloads[0].id, 'canonical-workload');
  assert.deepEqual(options[0].prepareSteps, [{ command: 'wordpress.wp-cli', args: ['command=option get home'] }]);
  assert.deepEqual(options[0].postSteps, [{ command: 'wordpress.browser-probe', args: ['url=/'] }]);
  assert.equal(Object.hasOwn(options[0], 'wp_codebox_workloads'), false);
  assert.deepEqual(JSON.parse(await readFile(results, 'utf8')).scenarios, []);
  assert.deepEqual(JSON.parse(await readFile(testResults, 'utf8')), { total: 1, passed: 1, failed: 0, skipped: 0 });
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox canonical CLI adapters smoke passed.');
