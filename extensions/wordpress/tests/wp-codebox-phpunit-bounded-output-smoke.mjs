/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
/**
 * Internal dependencies
 */
import { readBoundedText } from '../scripts/lib/wp-codebox-timeout-diagnostics.mjs';

const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-bounded-output-'));
const extension = path.resolve(import.meta.dirname, '..');
const adapter = path.join(extension, 'scripts/test/wp-codebox-phpunit-adapter.mjs');
const component = path.join(root, 'sample-plugin');
const cli = path.join(root, 'wp-codebox.mjs');
const belowLimit = path.join(root, 'below-limit.log');
const aboveLimit = path.join(root, 'above-limit.log');
const empty = path.join(root, 'empty.log');
const missing = path.join(root, 'missing.log');
const unreadable = path.join(root, 'directory');

await mkdir(path.join(component, 'tests'), { recursive: true });
await mkdir(unreadable);
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(belowLimit, 'under limit');
await writeFile(aboveLimit, '123456789');
await writeFile(empty, '');
await writeFile(cli, `
import { readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
if (args.includes('--version')) {
  process.stdout.write('0.21.0');
  process.exit(0);
}
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = await readFile(args[args.indexOf('--options') + 1], 'utf8');
  await writeFile(args[args.indexOf('--output') + 1], options);
  process.exit(0);
}
const options = JSON.parse(await readFile(args[args.indexOf('--recipe') + 1], 'utf8'));
if (process.env.BOUNDED_OUTPUT_FIXTURE === 'failure') {
  process.stdout.write('bounded discovery stdout ' + 'x'.repeat(9 * 1024) + ' END_UNBOUNDED');
  process.stderr.write('bounded discovery stderr');
  process.exit(7);
}
const discovery = {
  schema: 'wp-codebox/phpunit-discovery/v1',
  plugin_slug: options.pluginSlug,
  files: ['/wordpress/wp-content/plugins/sample-plugin/tests/SampleTest.php'],
};
process.stdout.write(JSON.stringify({
  executions: [{ command: 'wordpress.phpunit', stdout: JSON.stringify(discovery) }],
}));
`);

function runAdapter(fixture) {
  return spawnSync(process.execPath, [adapter], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_SETTINGS_JSON: '{}',
      HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY: '1',
      HOMEBOY_WP_CODEBOX_COMMAND_JSON: JSON.stringify([process.execPath, cli]),
      BOUNDED_OUTPUT_FIXTURE: fixture,
    },
    encoding: 'utf8',
  });
}

try {
  assert.deepEqual(await readBoundedText(belowLimit, 16), { text: 'under limit', truncated: false });
  assert.deepEqual(await readBoundedText(aboveLimit, 5), { text: '12345', truncated: true });
  assert.deepEqual(await readBoundedText(empty, 16), { text: '', truncated: false });
  assert.deepEqual(await readBoundedText(missing, 16), { text: '', truncated: false });
  assert.deepEqual(await readBoundedText(unreadable, 16), { text: '', truncated: false });

  const success = runAdapter('success');
  assert.equal(success.status, 0, success.stderr);
  assert.deepEqual(JSON.parse(success.stdout), {
    schema: 'wp-codebox/phpunit-discovery/v1',
    plugin_slug: 'sample-plugin',
    files: ['/wordpress/wp-content/plugins/sample-plugin/tests/SampleTest.php'],
  });

  const failure = runAdapter('failure');
  assert.equal(failure.status, 1, failure.stderr);
  assert.match(failure.stderr, /WP Codebox PHPUnit discovery failed with exit 7: bounded discovery stdout/);
  assert.match(failure.stderr, /bounded discovery stderr/);
  assert.doesNotMatch(failure.stderr, /END_UNBOUNDED/);
  assert.doesNotMatch(failure.stderr, /TypeError|\.trim is not a function|\[object Object\]/);
} finally {
  await rm(root, { recursive: true, force: true });
}

process.stdout.write('WP Codebox PHPUnit bounded output smoke passed.\n');
