import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-readiness-controller-'));
const executor = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs');
const providerReadiness = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'scripts', 'agent', 'homeboy-codebox-provider-readiness.cjs');
const coreModule = path.join(rootDir, 'wordpress', 'tests', 'fixtures', 'wp-codebox-core-agent-task-normalizer.mjs');

try {
  const overlayRoot = path.join(tempRoot, 'php-ai-client');
  fs.mkdirSync(overlayRoot, { recursive: true });
  fs.writeFileSync(path.join(overlayRoot, 'composer.json'), '{"name":"fixture/php-ai-client"}\n');

  const runner = path.join(tempRoot, 'runner.cjs');
  const capture = path.join(tempRoot, 'runner-called.json');
  fs.writeFileSync(runner, `#!/usr/bin/env node
'use strict';
require('node:fs').writeFileSync(${JSON.stringify(capture)}, 'called');
process.stdout.write(JSON.stringify({ success: true, status: 'completed' }));
`);
  fs.chmodSync(runner, 0o755);

  const request = {
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'runtime-readiness-controller-smoke',
    executor: {
      backend: 'wp-codebox',
      model: 'gpt-5.5',
      config: {
        provider: 'openai',
        provider_plugin_paths: ['/providers/openai'],
        runtime_overlays: [{
          kind: 'bundled-library',
          library: 'php-ai-client',
          source: overlayRoot,
          target: '/wordpress/wp-includes/php-ai-client',
        }],
      },
    },
    instructions: 'Verify runtime readiness preflight.',
    workspace: { mode: 'ephemeral' },
    limits: { timeout_ms: 120000 },
  };

  const configuredReadiness = spawnSync(process.execPath, [providerReadiness], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'homeboy/agent-task-provider-readiness-request/v1',
      provider_id: 'wordpress.codebox-agent-task-executor',
      backend: 'wp-codebox',
      effective_config: request.executor.config,
    }),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: coreModule,
    },
  });
  assert.equal(configuredReadiness.status, 0, configuredReadiness.stderr);
  const readinessResult = JSON.parse(configuredReadiness.stdout);
  assert.equal(readinessResult.schema, 'homeboy/agent-task-provider-readiness-result/v1');
  assert.equal(readinessResult.ready, false);
  assert.match(readinessResult.message, /vendor\/autoload\.php is missing/);

  const result = spawnSync(process.execPath, [executor, '--task-runner', runner], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: coreModule,
    },
  });

  assert.equal(result.status, 1, result.stderr || result.stdout);
  const outcome = JSON.parse(result.stdout);
  assert.equal(outcome.status, 'failed');
  assert.equal(outcome.failure_classification, 'provider');
  const diagnostic = outcome.diagnostics.find((entry) => entry.class === 'codebox.preflight.runtime_overlay_dependency_unprepared');
  assert.ok(diagnostic, 'runtime overlay dependency diagnostic is emitted');
  assert.match(diagnostic.message, /vendor\/autoload\.php is missing/);
  assert.equal(diagnostic.data.setup_command, `composer install --working-dir=${overlayRoot}`);
  assert.equal(diagnostic.data.owner_surface, 'wp-codebox-runtime-integration');
  assert.equal(fs.existsSync(capture), false, 'task runner is not spawned when runtime readiness fails');

  const injectionRequest = {
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'runtime-overlay-proof-injection',
    executor: {
      backend: 'wp-codebox',
      config: {
        runtime_overlay_proof: true,
      },
    },
    instructions: 'Reject a runner-injected mutable overlay.',
  };
  const injection = spawnSync(process.execPath, [executor, '--runtime-overlay-json', JSON.stringify({
    kind: 'library',
    source: overlayRoot,
    target: '/wordpress/injected',
  })], {
    encoding: 'utf8',
    input: JSON.stringify(injectionRequest),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: coreModule,
    },
  });
  assert.equal(injection.status, 1);
  assert.match(injection.stderr, /must declare a profile_id/);
} finally {
  fs.rmSync(tempRoot, { recursive: true, force: true });
}

console.log('wp-codebox runtime readiness controller smoke passed');
