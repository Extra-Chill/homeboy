#!/usr/bin/env node
'use strict';

const { findScenario, readJsonFile } = require('./lib/common.cjs');
const { assertLoopSuccess } = require('../../../runtime-agent-ci/lib/loop-lifecycle.cjs');

try {
  const scenario = findScenario(readJsonFile(process.env.RESULTS_FILE), process.env.FLOW_SLUG);
  const assertion = assertLoopSuccess({
    scenario,
    scenario_id: process.env.FLOW_SLUG,
    success_requires_pr: process.env.SUCCESS_REQUIRES_PR === 'true',
  });

  process.stdout.write(`job_status:                      ${assertion.job_status}\n`);
  process.stdout.write(`success_status:                  ${assertion.success_status}\n`);
  process.stdout.write(`error_message:                   ${assertion.error_message}\n`);
  process.stdout.write(`completion_outcome_satisfied:    ${assertion.completion_outcome_satisfied ? 'true' : 'false'}\n`);
  process.stdout.write(`no_changes_allowed:              ${assertion.no_changes_allowed ? 'true' : 'false'}\n`);
  process.exit(0);
} catch (error) {
  process.stderr.write(`ERROR: ${error.message}\n`);
  process.exit(1);
}
