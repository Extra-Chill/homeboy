'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(__dirname, '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

/**
 * Internal dependencies
 */
const { codeboxTaskRequestFromAgentTaskRequest } = require('../../agent-runtimes/wp-codebox/lib/codebox-agent-task-executor');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-runtime-overlay-defaults-'));
const previousPhpAiClientPath = process.env.HOMEBOY_WP_CODEBOX_PHP_AI_CLIENT_PATH;
const previousPlainPhpAiClientPath = process.env.PHP_AI_CLIENT_PATH;

try {
  const legacyPhpAiClientPath = path.join(root, 'php-ai-client@custom-provider-auth');
  const explicitPhpAiClientPath = path.join(root, 'php-ai-client-live');
  fs.mkdirSync(legacyPhpAiClientPath, { recursive: true });
  fs.mkdirSync(explicitPhpAiClientPath, { recursive: true });
  process.env.HOMEBOY_WP_CODEBOX_PHP_AI_CLIENT_PATH = legacyPhpAiClientPath;
  process.env.PHP_AI_CLIENT_PATH = legacyPhpAiClientPath;

  const request = {
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'runtime-overlay-defaults',
    instructions: 'Validate runtime overlay defaults.',
    executor: {
      backend: 'codebox',
      config: {
        provider: 'codex',
        runtime_task: {
          ability: 'example/run-task',
          input: {},
        },
      },
    },
  };

  const ambientPathRequest = codeboxTaskRequestFromAgentTaskRequest(request, {
    settings: {
      wp_codebox_php_ai_client_path: legacyPhpAiClientPath,
      php_ai_client_path: legacyPhpAiClientPath,
    },
  });
  assert.deepEqual(ambientPathRequest.runtime_overlays, []);
  assert(!JSON.stringify(ambientPathRequest).includes(legacyPhpAiClientPath));

  const explicitOverlay = {
    kind: 'bundled-library',
    library: 'php-ai-client',
    source: explicitPhpAiClientPath,
    target: '/wordpress/wp-includes/php-ai-client',
    strategy: 'wordpress-scoped-bundle',
  };
  const explicitOverlayRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        runtime_overlays: [explicitOverlay],
      },
    },
  }, {
    settings: {
      wp_codebox_php_ai_client_path: legacyPhpAiClientPath,
    },
  });
  assert.deepEqual(explicitOverlayRequest.runtime_overlays, [explicitOverlay]);
  assert(!JSON.stringify(explicitOverlayRequest.runtime_overlays).includes(legacyPhpAiClientPath));
} finally {
  if (previousPhpAiClientPath === undefined) {
    delete process.env.HOMEBOY_WP_CODEBOX_PHP_AI_CLIENT_PATH;
  } else {
    process.env.HOMEBOY_WP_CODEBOX_PHP_AI_CLIENT_PATH = previousPhpAiClientPath;
  }
  if (previousPlainPhpAiClientPath === undefined) {
    delete process.env.PHP_AI_CLIENT_PATH;
  } else {
    process.env.PHP_AI_CLIENT_PATH = previousPlainPhpAiClientPath;
  }
  fs.rmSync(root, { recursive: true, force: true });
}
