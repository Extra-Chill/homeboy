#!/usr/bin/env node
'use strict';

const fs = require('node:fs');

if (process.argv.includes('--provider-contract')) {
  const { agent_task_executors } = require('../../local-shell.json');
  process.stdout.write(`${JSON.stringify(agent_task_executors[0], null, 2)}\n`);
  process.exit(0);
}

const request = JSON.parse(fs.readFileSync(0, 'utf8'));
const taskId = request.task_id || 'local-shell-task';

process.stdout.write(`${JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: taskId,
  status: 'no_op',
  summary: 'Local shell runtime accepted the generic agent task request.',
  evidence_refs: [{ kind: 'preview', url: `https://example.test/${encodeURIComponent(taskId)}/preview` }],
  metadata: {
    runtime_id: 'local-shell',
    backend: request.executor?.backend || '',
  },
}, null, 2)}\n`);
