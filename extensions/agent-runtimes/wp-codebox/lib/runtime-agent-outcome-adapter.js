'use strict';

function scenarioResultsFromOutcome(outcome) {
  const codebox = outcome?.metadata?.codebox || {};
  const workload = codebox?.raw?.agent_runtime?.result
    || codebox?.agent_runtime?.workload
    || codebox?.agent_runtime?.result
    || codebox?.metadata?.agent_runtime?.workload
    || codebox?.metadata?.agent_runtime?.result
    || codebox?.agentResult
    || codebox?.agent_result
    || null;

  if (workload && Array.isArray(workload.scenarios)) {
    return workload;
  }
  return null;
}

module.exports = { scenarioResultsFromOutcome };
