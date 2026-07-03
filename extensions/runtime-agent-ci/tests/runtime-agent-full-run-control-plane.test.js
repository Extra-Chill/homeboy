'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  normalizeArtifactDownloads,
} = require('../../.github/scripts/runtime-agent-full-run/actions-artifact-downloads.cjs');
const {
  projectCallbackData,
} = require('../../.github/scripts/runtime-agent-full-run/project-callback-data.cjs');
const {
  materializeSecretEnv,
} = require('../../.github/scripts/runtime-agent-full-run/auth.cjs');

assert.deepEqual(normalizeArtifactDownloads(JSON.stringify([
  { runId: '123', artifactName: 'payload' },
  { repo: 'Extra-Chill/other', run_id: '456', name: 'report', destination: 'artifacts/report' },
]), 'Extra-Chill/example'), [
  { repo: 'Extra-Chill/example', run_id: '123', name: 'payload', dir: '.ci/actions-artifacts/payload' },
  { repo: 'Extra-Chill/other', run_id: '456', name: 'report', dir: 'artifacts/report' },
]);

assert.throws(
  () => normalizeArtifactDownloads(JSON.stringify([{ name: 'payload' }]), 'Extra-Chill/example'),
  /actions_artifact_downloads\[0\]\.run_id is required/
);

assert.deepEqual(projectCallbackData({ CALLBACK_DATA: '{"source":"workflow","attempt":2}' }), {
  callback_data_json: '{"source":"workflow","attempt":2}',
});

assert.deepEqual(projectCallbackData({ CALLBACK_DATA: 'null' }), {
  callback_data_json: '{}',
});

assert.deepEqual(projectCallbackData({ CALLBACK_DATA: 'false' }), {
  callback_data_json: '{}',
});

assert.throws(
  () => projectCallbackData({ CALLBACK_DATA: '[]' }),
  /Invalid callback_data: expected JSON object/
);

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-agent-auth-'));
try {
  const configPath = path.join(tmpRoot, 'config.json');
  const githubEnvPath = path.join(tmpRoot, 'github-env');
  fs.writeFileSync(configPath, JSON.stringify({
    secret_env: ['OPENAI_API_KEY'],
    secret_env_plan: {
      secret_env_names: ['ANTHROPIC_API_KEY'],
      requirements: [{ name: 'GITHUB_TOKEN', required: true }],
    },
  }));

  materializeSecretEnv({
    CONFIG_FILE: configPath,
    GITHUB_ENV: githubEnvPath,
    HOMEBOY_SECRET_ENV_BINDINGS: JSON.stringify({ OPENAI_API_KEY: 'UPSTREAM_OPENAI_API_KEY' }),
    UPSTREAM_OPENAI_API_KEY: 'declared-secret-value',
    UNDECLARED_SECRET: 'must-not-be-visible',
  });

  const githubEnv = fs.readFileSync(githubEnvPath, 'utf8');
  assert.match(githubEnv, /OPENAI_API_KEY<<HOMEBOY_SECRET_/);
  assert.match(githubEnv, /declared-secret-value/);
  assert.doesNotMatch(githubEnv, /UPSTREAM_OPENAI_API_KEY/);
  assert.doesNotMatch(githubEnv, /UNDECLARED_SECRET/);
  assert.doesNotMatch(githubEnv, /must-not-be-visible/);

  assert.throws(
    () => materializeSecretEnv({
      CONFIG_FILE: configPath,
      GITHUB_ENV: githubEnvPath,
      HOMEBOY_SECRET_ENV_BINDINGS: JSON.stringify({ UNDECLARED_SECRET: 'UNDECLARED_SECRET' }),
      UNDECLARED_SECRET: 'must-not-be-visible',
    }),
    /secret_env_bindings\.UNDECLARED_SECRET must target a declared secret env name/
  );

  assert.throws(
    () => materializeSecretEnv({
      CONFIG_FILE: configPath,
      GITHUB_ENV: githubEnvPath,
      HOMEBOY_SECRET_ENV_BINDINGS: JSON.stringify({ ANTHROPIC_API_KEY: 'MISSING_ANTHROPIC_API_KEY' }),
    }),
    /secret_env_bindings\.ANTHROPIC_API_KEY source env MISSING_ANTHROPIC_API_KEY is not set/
  );

  assert.throws(
    () => materializeSecretEnv({
      CONFIG_FILE: configPath,
      GITHUB_ENV: githubEnvPath,
      HOMEBOY_GITHUB_SECRETS_JSON: JSON.stringify({ OPENAI_API_KEY: 'legacy-secret-value' }),
    }),
    /requires non-empty secret_env_bindings/
  );
} finally {
  fs.rmSync(tmpRoot, { recursive: true, force: true });
}

process.stdout.write('Runtime agent full-run control-plane projection checks passed\n');
