'use strict';

function scenarioResultsFromOutcome(outcome) {
  const workload = outcome?.metadata?.codebox?.artifact_result?.result || null;

  if (workload && Array.isArray(workload.scenarios)) {
    return workload;
  }
  return null;
}

module.exports = { scenarioResultsFromOutcome };
