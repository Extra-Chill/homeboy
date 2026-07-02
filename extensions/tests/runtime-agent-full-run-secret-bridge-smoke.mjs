#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const runnerTemp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-secret-bridge-runner-'));
const githubEnvPath = path.join(runnerTemp, 'github-env');
const bridgeConfigPath = path.join(runnerTemp, 'secret-bridge-config.json');
const emptyConfigPath = path.join(runnerTemp, 'secret-bridge-empty-config.json');

fs.writeFileSync(bridgeConfigPath, `${JSON.stringify({
  secret_env_map: {
    OPENAI_API_KEY: ['PROVIDER_SECRET_1', 'UNSET_PROVIDER_SECRET'],
  },
}, null, 2)}\n`);

const bridgeResult = spawnSync(process.execPath, [
  path.join(repoRoot, '.github/scripts/runtime-agent-full-run/auth.cjs'),
  'materialize-secret-env',
], {
  cwd: repoRoot,
  env: {
    ...process.env,
    CONFIG_FILE: bridgeConfigPath,
    GITHUB_ENV: githubEnvPath,
    HOMEBOY_GITHUB_SECRETS_JSON: JSON.stringify({
      PROVIDER_SECRET_1: 'github-secret-value',
      UNDECLARED_SECRET: 'must-not-materialize',
    }),
  },
  encoding: 'utf8',
});

assert.equal(bridgeResult.status, 0, bridgeResult.stderr || bridgeResult.stdout);
const githubEnv = fs.readFileSync(githubEnvPath, 'utf8');
assert.match(githubEnv, /PROVIDER_SECRET_1<</);
assert.match(githubEnv, /github-secret-value/);
assert.equal(githubEnv.includes('UNDECLARED_SECRET'), false);
assert.equal(githubEnv.includes('must-not-materialize'), false);

fs.writeFileSync(emptyConfigPath, `${JSON.stringify({}, null, 2)}\n`);
const missingPlanResult = spawnSync(process.execPath, [
  path.join(repoRoot, '.github/scripts/runtime-agent-full-run/auth.cjs'),
  'materialize-secret-env',
], {
  cwd: repoRoot,
  env: {
    ...process.env,
    CONFIG_FILE: emptyConfigPath,
    GITHUB_ENV: path.join(runnerTemp, 'github-env-empty'),
    HOMEBOY_GITHUB_SECRETS_JSON: '{}',
  },
  encoding: 'utf8',
});
assert.notEqual(missingPlanResult.status, 0, missingPlanResult.stderr || missingPlanResult.stdout);
assert.match(missingPlanResult.stderr, /requires a non-empty secret_env_map/);

console.log('runtime agent full-run secret bridge smoke passed');
