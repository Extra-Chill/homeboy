#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { buildConfig } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/build-runner-config.cjs'));
const { resolveControllerLoopProofPolicy } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/lib/proof-profile.cjs'));
const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-local-shell-workspace-'));
const runnerTemp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-local-shell-runner-'));
const configPath = path.join(runnerTemp, 'runtime-agent-full-run-config.json');
const resultsPath = path.join(runnerTemp, 'run-results.json');
const outcomePath = path.join(runnerTemp, 'agent-task-outcome.json');

fs.writeFileSync(configPath, `${JSON.stringify({
  workload_id: 'local-shell-smoke',
  workload_label: 'Local shell smoke',
  component_path: workspace,
  target_repo: 'Extra-Chill/example-target',
  runtime_id: 'local-shell',
  runtime_profile: 'local-shell-ci',
  runtime_profiles: {
    'local-shell-ci': {
      id: 'local-shell-ci',
      runtime_task_ability: 'local-shell/run-task',
    },
  },
  runtime_requirements: {
    id: 'local-shell-ci',
    runtime_task_ability: 'local-shell/run-task',
  },
  runtime_task: {
    ability: 'local-shell/run-task',
    input: { message: 'prove generic runtime path' },
  },
  prompt: 'Accept the generic runtime request.',
  success_requires_pr: false,
}, null, 2)}\n`);

const result = spawnSync(process.execPath, [
  path.join(repoRoot, '.github/scripts/runtime-agent-full-run/run-runtime-agent-task.cjs'),
  configPath,
], {
  cwd: workspace,
  env: {
    ...process.env,
    HOMEBOY_RUNTIME_AGENT_RESULTS_FILE: resultsPath,
    HOMEBOY_AGENT_TASK_OUTCOME_FILE: outcomePath,
  },
  encoding: 'utf8',
});

assert.equal(result.status, 0, result.stderr || result.stdout);
const outcome = JSON.parse(fs.readFileSync(outcomePath, 'utf8'));
assert.equal(outcome.status, 'no_op');
assert.equal(outcome.metadata.runtime_id, 'local-shell');
assert.equal(outcome.metadata.backend, 'local-shell');
assert.equal(outcome.evidence_refs[0].kind, 'preview');

const results = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
assert.equal(results.scenarios[0].id, 'local-shell-smoke');
assert.equal(results.scenarios[0].metadata.job_status, 'no_op');
assert.equal(results.scenarios[0].metadata.controller_loop_proof_validation.valid, true);
assert.equal(results.scenarios[0].metadata.bounded_production_loop_proof.status, 'succeeded');

assert.deepEqual(resolveControllerLoopProofPolicy({}), {
  proof_profile: 'artifact_only',
  preview_required: false,
  publication_required: false,
});
assert.deepEqual(resolveControllerLoopProofPolicy({ proof_profile: 'cook_to_pr' }), {
  proof_profile: 'cook_to_pr',
  preview_required: true,
  publication_required: true,
  publication_evidence: { kind: 'pull_request' },
});
assert.deepEqual(resolveControllerLoopProofPolicy({ proof_profile: 'none' }), {
  proof_profile: 'none',
  preview_required: false,
  publication_required: false,
  artifacts: [],
  required_evidence: [],
});
assert.equal(resolveControllerLoopProofPolicy({
  proof_profile: 'cook_to_pr',
  controller_loop_proof_policy: { preview_required: false },
}).preview_required, false);
assert.throws(() => resolveControllerLoopProofPolicy({ proof_profile: 'unsupported' }), /Unsupported proof_profile/);

const forwardedConfig = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'proof-profile-forwarding',
  COMPONENT_ID: 'example-component',
  TARGET_REPO: 'Extra-Chill/example-target',
  RUNTIME: 'local-shell',
  PROFILE: 'local-shell-ci',
  PROOF_PROFILE: 'cook_to_pr',
});
assert.equal(forwardedConfig.proof_profile, 'cook_to_pr');

console.log('runtime agent full-run local-shell smoke passed');
