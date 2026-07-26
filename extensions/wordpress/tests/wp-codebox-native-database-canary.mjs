/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { access, chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const cli = process.env.WP_CODEBOX_NATIVE_CANARY_BIN;
if (!cli) {
  console.log('WP Codebox native database canary skipped: WP_CODEBOX_NATIVE_CANARY_BIN is not set.');
  process.exit(0);
}

const descriptorRun = spawnSync(cli, ['runtime', 'descriptor', '--json'], { encoding: 'utf8' });
let descriptor;
try {
  descriptor = descriptorRun.status === 0 ? JSON.parse(descriptorRun.stdout) : null;
} catch {
  descriptor = null;
}
const capability = 'runtime-service:mysql:native:mariadb';
assert.equal(descriptor?.schema, 'wp-codebox/runtime-descriptor/v1', 'candidate CLI must expose the public runtime descriptor');
assert.equal(descriptor?.capabilities?.includes(capability), true, 'candidate CLI must advertise native MariaDB support');
assert.equal(descriptor?.contractManifest?.capabilities?.runtimeServices?.schema, 'wp-codebox/runtime-service-capabilities/v1');
assert.equal(descriptor?.contractManifest?.capabilities?.runtimeServices?.capabilities?.includes(capability), true);

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-native-database-canary-'));
const component = path.join(root, 'native-database-canary');
const artifacts = path.join(root, 'artifacts');
const routingGuardBin = path.join(root, 'routing-guard-bin');
const dockerMarker = path.join(root, 'docker-selected');
try {
  await mkdir(path.join(component, 'tests'), { recursive: true });
  await mkdir(routingGuardBin, { recursive: true });
  const dockerGuard = path.join(routingGuardBin, 'docker');
  await writeFile(dockerGuard, '#!/bin/sh\n: > "$WP_CODEBOX_NATIVE_CANARY_DOCKER_MARKER"\nexit 97\n');
  await chmod(dockerGuard, 0o755);
  await writeFile(path.join(component, 'native-database-canary.php'), '<?php\n/* Plugin Name: Native Database Canary */\n');
  await writeFile(path.join(component, 'tests/test-native-database.php'), `<?php
class Native_Database_Canary_Test extends WP_UnitTestCase {
    public function test_managed_mysql_connection(): void {
        global $wpdb;
        $version = $wpdb->get_var( 'SELECT VERSION()' );
        $this->assertIsString( $version );
        $this->assertStringContainsString( 'MariaDB', $version );
    }
}
`);

  const run = spawnSync(runner, [], {
    env: {
      ...process.env,
      PATH: `${routingGuardBin}${path.delimiter}${process.env.PATH || ''}`,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'native-database-canary',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      WP_CODEBOX_NATIVE_CANARY_DOCKER_MARKER: dockerMarker,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        database_type: 'mysql',
        wp_codebox_database_service: { provider: 'native', engine: 'mariadb' },
      }),
    },
    encoding: 'utf8',
    timeout: 180_000,
  });
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
  await assert.rejects(access(dockerMarker), { code: 'ENOENT' }, 'native provider routing must not invoke Docker');
  const runDirectory = (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  assert.ok(runDirectory, 'adapter must retain the WP Codebox run artifacts');
  const pointer = JSON.parse(await readFile(path.join(artifacts, runDirectory, 'latest-runtime.json'), 'utf8'));
  const services = pointer.managedRuntimeServices;
  assert.equal(services?.length, 1, 'final artifact pointer must expose one managed database service');
  assert.equal(services[0].provider, 'native');
  assert.equal(services[0].lifecycle, 'released');
  assert.equal(services[0].teardown, 'completed');
  const allowedEvidenceFields = new Set(['id', 'kind', 'provider', 'version', 'readiness', 'lifecycle', 'teardown', 'diagnostic', 'controls', 'memory']);
  assert.deepEqual(Object.keys(services[0]).filter((key) => !allowedEvidenceFields.has(key)), [], 'lifecycle evidence must remain bounded');
  const serializedEvidence = JSON.stringify(services);
  assert.doesNotMatch(serializedEvidence, /(?:password|credential|secret|token|socket|datadir|pid.file|log.file)/i);
  assert.doesNotMatch(serializedEvidence, /(?:\/tmp\/|wp-codebox-mariadb-)/);
  const runtimeDirectory = pointer.paths.runtimeDirectory;
  const handedOff = JSON.parse(await readFile(path.join(artifacts, runDirectory, runtimeDirectory, 'files/managed-runtime-services.json'), 'utf8'));
  assert.deepEqual(handedOff, services, 'adapter must hand off the final upstream lifecycle evidence without rewriting it');
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox native database canary passed.');
