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
      backend: 'wp-codebox',
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

  const overlaySha = 'a'.repeat(40);
  const phpAiClientProfile = {
    schema: 'homeboy/runtime-overlay-profile/v1',
    id: 'php-ai-client-provider-metadata',
    repository: { identity: 'example/php-ai-client', ref: overlaySha, sha: overlaySha },
    source: explicitPhpAiClientPath,
    target: '/wordpress/wp-includes/php-ai-client',
    required_capabilities: ['php-ai-client.provider-metadata.get-description'],
    preparation_evidence: {
      checkout: { repository_identity: 'example/php-ai-client', ref: overlaySha, sha: overlaySha, clean: true },
      probes: [{ capability: 'php-ai-client.provider-metadata.get-description', command: ['true'] }],
    },
  };
  const providerProfile = {
    schema: 'homeboy/runtime-overlay-profile/v1',
    id: 'provider-overlay',
    repository: { identity: 'example/provider', ref: overlaySha, sha: overlaySha },
    source: path.join(root, 'provider-overlay'),
    target: '/wordpress/wp-content/plugins/provider-overlay',
    required_capabilities: ['provider.registration'],
    preparation_evidence: {
      checkout: { repository_identity: 'example/provider', ref: overlaySha, sha: overlaySha, clean: true },
      probes: [{ capability: 'provider.registration', command: ['true'] }],
    },
  };
  fs.mkdirSync(providerProfile.source, { recursive: true });
  const composedOverlayRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        runtime_overlay_profiles: [phpAiClientProfile, providerProfile],
        runtime_overlays: [
          { ...explicitOverlay, profile_id: phpAiClientProfile.id },
          { kind: 'plugin', source: providerProfile.source, target: providerProfile.target, profile_id: providerProfile.id },
        ],
      },
    },
  });
  assert.deepEqual(composedOverlayRequest.runtime_overlays.map((overlay) => overlay.profile_id), [
    'php-ai-client-provider-metadata',
    'provider-overlay',
  ]);
  assert.deepEqual(composedOverlayRequest.runtime_overlays.map((overlay) => overlay.target), [
    phpAiClientProfile.target,
    providerProfile.target,
  ]);

  const materializedOverlayRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        runtime_overlay_profiles: [phpAiClientProfile],
        runtime_overlays: [{ kind: 'bundled-library', profile_id: phpAiClientProfile.id }],
      },
    },
  });
  assert.deepEqual(materializedOverlayRequest.runtime_overlays[0].source, phpAiClientProfile.source);
  assert.deepEqual(materializedOverlayRequest.runtime_overlays[0].target, phpAiClientProfile.target);
  assert.deepEqual(composedOverlayRequest.context.runtime_overlay_profiles.map((profile) => profile.repository.identity), [
    'example/php-ai-client',
    'example/provider',
  ]);

  assert.throws(() => codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        runtime_overlay_profiles: [phpAiClientProfile],
        runtime_overlays: [{ ...explicitOverlay, profile_id: phpAiClientProfile.id, target: '/wordpress/wrong-target' }],
      },
    },
  }), /Overlay target must match profile/);

  assert.throws(() => codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        runtime_overlay_proof: true,
        runtime_overlays: [explicitOverlay],
      },
    },
  }), /must declare a profile_id/);

  const clearedRunnerDefaultsRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        runtime_env: {},
        runtime_overlays: [],
      },
    },
  }, {
    settings: {
      runtime_env: {
        WP_CODEBOX_PHP_AI_CLIENT_PATH: legacyPhpAiClientPath,
      },
      runtime_overlays: [explicitOverlay],
    },
  });
  assert.deepEqual(clearedRunnerDefaultsRequest.runtime_overlays, []);
  assert.equal(clearedRunnerDefaultsRequest.runtime_env.WP_CODEBOX_PHP_AI_CLIENT_PATH, undefined);
  assert(!JSON.stringify(clearedRunnerDefaultsRequest).includes(legacyPhpAiClientPath));
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
