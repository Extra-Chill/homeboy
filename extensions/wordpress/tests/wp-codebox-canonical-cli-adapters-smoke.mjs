import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-canonical-adapter-'));
const component = path.join(root, 'plugin');
const observed = path.join(root, 'observed.jsonl');
const cli = path.join(root, 'wp-codebox');
const results = path.join(root, 'results', 'bench.json');
await mkdir(component, { recursive: true });
await writeFile(cli, `#!/usr/bin/env node
import { appendFile, readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
await appendFile(process.env.OBSERVED, JSON.stringify(args) + '\\n');
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = JSON.parse(await readFile(args[args.indexOf('--options') + 1], 'utf8'));
  await appendFile(process.env.OBSERVED, JSON.stringify({ options }) + '\\n');
  await writeFile(args[args.indexOf('--output') + 1], JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1' }));
  process.exit(0);
}
if (args[0] === 'recipe-run') process.stdout.write(JSON.stringify({ success: true, benchResults: { schema: 'homeboy/bench-results/v1', scenarios: [] } }));
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
      wordpress_runtime_workloads: [{ id: 'canonical-workload', run: [] }],
      wordpress_runtime_blueprint: { steps: [] },
    }),
  };
  const extension = path.resolve(import.meta.dirname, '..');
  const bench = spawnSync(path.join(extension, 'scripts/bench/bench-runner-wp-codebox.sh'), [], { env: { ...process.env, ...base, HOMEBOY_BENCH_RESULTS_FILE: results }, encoding: 'utf8' });
  assert.equal(bench.status, 0, bench.stderr);
  const phpunit = spawnSync(path.join(extension, 'scripts/test/test-runner-wp-codebox.sh'), [], { env: { ...process.env, ...base }, encoding: 'utf8' });
  assert.equal(phpunit.status, 0, phpunit.stderr);
  const entries = (await readFile(observed, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.deepEqual(entries.filter(Array.isArray).map((args) => args.slice(0, 2)), [
    ['recipe', 'build'], ['recipe-run', '--recipe'], ['recipe', 'build'], ['recipe-run', '--recipe'],
  ]);
  const options = entries.filter((entry) => entry.options).map((entry) => entry.options);
  assert.deepEqual(options.map((entry) => entry.extra_plugins[0]), [
    { source: '/workspace/monorepo', sourceSubpath: 'plugins/canonical-plugin', slug: 'canonical-plugin', activate: false },
    { source: '/workspace/monorepo', sourceSubpath: 'plugins/canonical-plugin', slug: 'canonical-plugin', activate: false },
  ]);
  assert.equal(options[0].workloads[0].id, 'canonical-workload');
  assert.equal(Object.hasOwn(options[0], 'wp_codebox_workloads'), false);
  assert.deepEqual(JSON.parse(await readFile(results, 'utf8')).scenarios, []);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox canonical CLI adapters smoke passed.');
