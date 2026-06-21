#!/usr/bin/env node
'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const request = JSON.parse(fs.readFileSync(0, 'utf8'));
const artifactDir = process.env.HOMEBOY_HEADLESS_FIXTURE_ARTIFACT_DIR || fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-headless-fixture-'));
fs.mkdirSync(artifactDir, { recursive: true });

const evidence = {
  schema: 'homeboy/headless-deterministic-loop-evidence/v1',
  task_id: request.task_id,
  executor_backend: request.executor?.backend || '',
  expected_artifacts: request.expected_artifacts || [],
  observed_request_schema: request.schema,
};
const evidencePath = path.join(artifactDir, `${request.task_id}-evidence.json`);
const evidenceJson = `${JSON.stringify(evidence, null, 2)}\n`;
fs.writeFileSync(evidencePath, evidenceJson);

const artifact = {
  schema: 'homeboy/deterministic-loop-artifact/v1',
  id: `${request.task_id}-evidence`,
  name: 'headless-fixture-evidence',
  path: evidencePath,
  sha256: crypto.createHash('sha256').update(evidenceJson).digest('hex'),
  metadata: {
    evidence_schema: evidence.schema,
    content_type: 'application/json',
  },
};

process.stdout.write(`${JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: request.task_id,
  status: 'succeeded',
  summary: 'Headless deterministic loop fixture completed.',
  artifacts: [artifact],
  metadata: {
    evidence,
    agent_loop_results: {
      scenarios: [{
        id: request.task_id,
        label: 'Headless deterministic loop fixture',
        metrics: { generic_agent_task_executor_mean: 1 },
        metadata: {
          job_status: 'completed',
          success_status: 'no_changes',
          completion_outcome: 'fixture_completed',
          completion_outcome_satisfied: true,
          evidence_schema: evidence.schema,
          artifact_count: 1,
        },
      }],
    },
  },
}, null, 2)}\n`);
