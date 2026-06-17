#!/usr/bin/env node
'use strict';

const { findScenario, readJsonFile } = require('./lib/common.cjs');

try {
  const scenario = findScenario(readJsonFile(process.env.RESULTS_FILE), process.env.FLOW_SLUG);
  const metadata = scenario.metadata || {};
  const jobStatus = metadata.job_status || '';
  const successStatus = metadata.success_status || '';
  const errorMessage = metadata.error_message || '';
  const completionOutcomeSatisfied = metadata.completion_outcome_satisfied === true;
  const noChangesAllowed = process.env.SUCCESS_REQUIRES_PR !== 'true';

  process.stdout.write(`job_status:                      ${jobStatus}\n`);
  process.stdout.write(`success_status:                  ${successStatus}\n`);
  process.stdout.write(`error_message:                   ${errorMessage}\n`);
  process.stdout.write(`completion_outcome_satisfied:    ${completionOutcomeSatisfied ? 'true' : 'false'}\n`);
  process.stdout.write(`no_changes_allowed:              ${noChangesAllowed ? 'true' : 'false'}\n`);

  if (errorMessage) {
    throw new Error(`scenario ${process.env.FLOW_SLUG} completed with error_message=${errorMessage}`);
  }
  if (successStatus === 'pr_opened' || completionOutcomeSatisfied || (successStatus === 'no_changes' && noChangesAllowed)) {
    process.exit(0);
  }
  throw new Error(`scenario ${process.env.FLOW_SLUG} expected opened PR, satisfied completion outcome, or allowed no-changes result, got job_status=${jobStatus} success_status=${successStatus} completion_outcome_satisfied=${completionOutcomeSatisfied ? 'true' : 'false'} no_changes_allowed=${noChangesAllowed ? 'true' : 'false'}`);
} catch (error) {
  process.stderr.write(`ERROR: ${error.message}\n`);
  process.exit(1);
}
