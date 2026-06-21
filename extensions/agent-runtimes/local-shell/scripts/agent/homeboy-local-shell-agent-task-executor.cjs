#!/usr/bin/env node
'use strict';

const fs = require('node:fs');

const request = JSON.parse(fs.readFileSync(0, 'utf8'));
const taskId = request.task_id || 'local-shell-task';

process.stdout.write(`${JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: taskId,
  status: 'no_op',
  summary: 'Local shell runtime accepted the generic agent task request.',
  metadata: {
    runtime_id: 'local-shell',
    backend: request.executor?.backend || '',
  },
}, null, 2)}\n`);
