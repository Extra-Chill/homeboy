import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-readiness-controller-'));
const executor = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs');
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
} finally {
  fs.rmSync(tempRoot, { recursive: true, force: true });
}

console.log('wp-codebox runtime readiness controller smoke passed');
