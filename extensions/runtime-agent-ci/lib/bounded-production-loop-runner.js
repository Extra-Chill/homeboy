'use strict';

const { runGenericDeterministicLoop } = require('./generic-agent-loop-runner');

const BOUNDED_PRODUCTION_LOOP_RESULT_SCHEMA = 'homeboy/bounded-production-loop-result/v1';
const BOUNDED_PRODUCTION_LOOP_ITERATION_SCHEMA = 'homeboy/bounded-production-loop-iteration/v1';
const BOUNDED_PRODUCTION_LOOP_EVIDENCE_SCHEMA = 'homeboy/bounded-production-loop-evidence/v1';

function runBoundedProductionLoop(options = {}) {
  const loopId = options.loopId || options.loop_id || 'bounded-production-loop';
  const maxIterations = positiveInteger(options.maxIterations || options.max_iterations, 1);
  const executeIteration = requiredFunction(options.executeIteration || options.execute_iteration || options.execute, 'executeIteration');
  const buildIteration = options.buildIteration || options.build_iteration || defaultBuildIteration;
  const collectResult = options.collectResult || options.collect_result || defaultCollectResult;
  const projectFinalState = options.projectFinalState || options.project_final_state || defaultProjectFinalState;
  const stopCriteria = options.stopCriteria || options.stop_criteria || defaultStopCriteria;
  const repairPolicy = options.repairPolicy || options.repair_policy || null;
  const fanoutPolicy = options.fanoutPolicy || options.fanout_policy || null;
  const policy = normalizePolicy(options);
  const productionIterations = [];
  let stopReason = '';
  let validationFailures = [];

  const loop = runGenericDeterministicLoop({
    loopId,
    maxIterations,
    state: optionalObject(options.state || options.initialState || options.initial_state),
    buildTask: (context) => buildIteration({ ...context, policy }),
    executeTask: (context) => executeIteration({ ...context, policy }),
    collectResult: (context) => collectResult({ ...context, policy }),
    reconcile: (context) => {
      const evidence = collectProductionEvidence(context);
      const failures = validateProductionResult({ ...context, evidence, policy });
      const accepted = failures.length === 0 && isAcceptedResult(context.result, policy);
      const repair = typeof repairPolicy === 'function' ? repairPolicy({ ...context, evidence, failures, policy, accepted }) : null;
      const fanout = typeof fanoutPolicy === 'function' ? fanoutPolicy({ ...context, evidence, failures, policy, accepted }) : null;
      const record = {
        schema: BOUNDED_PRODUCTION_LOOP_ITERATION_SCHEMA,
        loop_id: loopId,
        iteration: context.iteration,
        task: context.task,
        result: context.result,
        artifacts: evidence.artifacts,
        evidence_refs: evidence.evidence_refs,
        validation_failures: failures,
        accepted,
        repair,
        fanout,
      };
      productionIterations.push(record);
      validationFailures = failures;
      return {
        ...context.state,
        latest_result: context.result,
        latest_validation_failures: failures,
        accepted,
        repair,
        fanout,
        production_iterations: productionIterations,
      };
    },
    stopPolicy: (context) => {
      const latest = productionIterations[productionIterations.length - 1];
      if (latest?.accepted) {
        stopReason = 'accepted';
        return { stop: true, reason: stopReason };
      }
      const decision = normalizeStopDecision(stopCriteria({ ...context, production_iteration: latest, validation_failures: validationFailures, policy }));
      if (decision.stop) {
        stopReason = decision.reason || 'stop_criteria_satisfied';
        return decision;
      }
      if (context.iteration >= maxIterations) {
        stopReason = 'max_iterations_reached';
        return { stop: true, reason: stopReason };
      }
      return { stop: false };
    },
    shouldContinue: (context) => {
      const latest = productionIterations[productionIterations.length - 1];
      return !latest?.accepted && context.iteration < maxIterations;
    },
  });

  const finalState = projectFinalState({
    loop_id: loopId,
    loop,
    state: loop.state,
    iterations: productionIterations,
    latest_result: productionIterations[productionIterations.length - 1]?.result || null,
    validation_failures: validationFailures,
    policy,
  });
  const status = productionIterations.at(-1)?.accepted ? 'succeeded' : 'failed';
  return {
    schema: BOUNDED_PRODUCTION_LOOP_RESULT_SCHEMA,
    loop_id: loopId,
    status,
    stop_reason: stopReason || (productionIterations.length === 0 ? 'empty' : 'unknown'),
    max_iterations: maxIterations,
    iteration_count: productionIterations.length,
    iterations: productionIterations,
    validation_failures: validationFailures,
    evidence_envelope: buildEvidenceEnvelope({ loopId, status, iterations: productionIterations, policy }),
    final_state: finalState,
    deterministic_loop: loop,
  };
}

function defaultBuildIteration({ state }) {
  return state.task || state.input || state.request || state;
}

function defaultCollectResult({ outcome }) {
  return outcome;
}

function defaultProjectFinalState({ state }) {
  return state;
}

function defaultStopCriteria() {
  return { stop: false };
}

function normalizePolicy(options) {
  return {
    accepted_statuses: normalizeArray(options.acceptedStatuses || options.accepted_statuses).length > 0
      ? normalizeArray(options.acceptedStatuses || options.accepted_statuses)
      : ['accepted', 'succeeded', 'passed'],
    validation_gates: normalizeArray(options.validationGates || options.validation_gates),
    artifact_requirements: normalizeRequirements(options.artifactRequirements || options.artifact_requirements),
    evidence_requirements: [
      ...normalizeRequirements(options.evidenceRequirements || options.evidence_requirements),
      ...requirementFromFlag(options.previewRequirement ?? options.preview_requirement ?? options.requirePreview ?? options.require_preview, 'preview', 'preview'),
      ...requirementFromFlag(options.publicationEvidenceRequirement ?? options.publication_evidence_requirement ?? options.requirePublicationEvidence ?? options.require_publication_evidence ?? options.requirePrEvidence ?? options.require_pr_evidence, 'publication', 'publication'),
    ],
  };
}

function normalizeRequirements(value) {
  return normalizeArray(value)
    .map((requirement) => typeof requirement === 'string' ? { name: requirement } : requirement)
    .filter(isPlainObject)
    .map((requirement) => ({
      name: requirement.name || requirement.id || '',
      kind: requirement.kind || requirement.type || '',
      role: requirement.role || '',
      required: requirement.optional === true ? false : requirement.required !== false,
      optional: requirement.optional === true || requirement.required === false,
    }));
}

function requirementFromFlag(value, name, kind) {
  if (value === true) {
    return [{ name, kind, required: true, optional: false }];
  }
  if (isPlainObject(value)) {
    return normalizeRequirements([{ name, kind, ...value, required: value.optional === true ? false : value.required !== false }]);
  }
  return [];
}

function collectProductionEvidence({ outcome, result, artifacts }) {
  const sourceArtifacts = [
    ...normalizeArray(artifacts),
    ...normalizeArray(outcome?.artifacts),
    ...normalizeArray(result?.artifacts),
  ];
  const evidenceRefs = [
    ...normalizeArray(outcome?.evidence_refs || outcome?.evidence),
    ...normalizeArray(result?.evidence_refs || result?.evidence),
  ];
  return { artifacts: dedupeRecords(sourceArtifacts), evidence_refs: dedupeRecords(evidenceRefs) };
}

function validateProductionResult(context) {
  const failures = [];
  for (const gate of context.policy.validation_gates) {
    const failure = validateGate(gate, context);
    if (failure) {
      failures.push(failure);
    }
  }
  for (const requirement of context.policy.artifact_requirements) {
    if (requirement.required && !findMatchingRecord(context.evidence.artifacts, requirement)) {
      failures.push({ code: 'missing_required_artifact', message: `missing required artifact ${requirementLabel(requirement)}`, requirement });
    }
  }
  for (const requirement of context.policy.evidence_requirements) {
    if (requirement.required && !findMatchingRecord(context.evidence.evidence_refs, requirement)) {
      failures.push({ code: 'missing_required_evidence', message: `missing required evidence ${requirementLabel(requirement)}`, requirement });
    }
  }
  return failures;
}

function validateGate(gate, context) {
  if (typeof gate === 'function') {
    return normalizeGateFailure(gate(context), 'validation_gate_failed');
  }
  if (isPlainObject(gate) && typeof gate.validate === 'function') {
    return normalizeGateFailure(gate.validate(context), gate.name || gate.id || 'validation_gate_failed');
  }
  return null;
}

function normalizeGateFailure(value, name) {
  if (value === true || value === undefined || value === null) {
    return null;
  }
  if (value === false) {
    return { code: 'validation_gate_failed', message: `${name} failed`, gate: name };
  }
  if (typeof value === 'string') {
    return { code: 'validation_gate_failed', message: value, gate: name };
  }
  if (isPlainObject(value)) {
    if (value.pass === true || value.passed === true || value.ok === true) {
      return null;
    }
    return {
      code: value.code || 'validation_gate_failed',
      message: value.message || `${name} failed`,
      gate: value.gate || name,
      data: value.data,
    };
  }
  return null;
}

function isAcceptedResult(result, policy) {
  return policy.accepted_statuses.includes(result?.status) || policy.accepted_statuses.includes(result?.state);
}

function buildEvidenceEnvelope({ loopId, status, iterations, policy }) {
  return {
    schema: BOUNDED_PRODUCTION_LOOP_EVIDENCE_SCHEMA,
    loop_id: loopId,
    status,
    iteration_count: iterations.length,
    required_artifacts: policy.artifact_requirements.filter((requirement) => requirement.required),
    optional_artifacts: policy.artifact_requirements.filter((requirement) => !requirement.required),
    required_evidence: policy.evidence_requirements.filter((requirement) => requirement.required),
    optional_evidence: policy.evidence_requirements.filter((requirement) => !requirement.required),
    iterations: iterations.map((iteration) => ({
      iteration: iteration.iteration,
      accepted: iteration.accepted,
      artifact_count: iteration.artifacts.length,
      evidence_ref_count: iteration.evidence_refs.length,
      validation_failures: iteration.validation_failures,
      artifacts: iteration.artifacts,
      evidence_refs: iteration.evidence_refs,
    })),
  };
}

function findMatchingRecord(records, requirement) {
  return normalizeArray(records).find((record) => {
    if (!record || typeof record !== 'object') {
      return false;
    }
    return Boolean(
      (requirement.name && [record.name, record.id, record.label].includes(requirement.name))
      || (requirement.kind && [record.kind, record.type].includes(requirement.kind))
      || (requirement.role && record.role === requirement.role)
    );
  });
}

function requirementLabel(requirement) {
  return requirement.name || requirement.kind || requirement.role || '(unnamed)';
}

function dedupeRecords(records) {
  const seen = new Set();
  const deduped = [];
  for (const record of normalizeArray(records)) {
    if (!record || typeof record !== 'object') {
      continue;
    }
    const key = record.url || record.uri || record.path || record.name || record.id || JSON.stringify(record);
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    deduped.push(record);
  }
  return deduped;
}

function normalizeStopDecision(value) {
  if (isPlainObject(value)) {
    return { stop: Boolean(value.stop), reason: value.reason || '', data: value.data };
  }
  return { stop: Boolean(value), reason: Boolean(value) ? 'stop_criteria_satisfied' : '' };
}

function positiveInteger(value, fallback) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function requiredFunction(value, name) {
  if (typeof value !== 'function') {
    throw new Error(`${name} must be a function.`);
  }
  return value;
}

function optionalObject(value) {
  return isPlainObject(value) ? value : {};
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  BOUNDED_PRODUCTION_LOOP_EVIDENCE_SCHEMA,
  BOUNDED_PRODUCTION_LOOP_ITERATION_SCHEMA,
  BOUNDED_PRODUCTION_LOOP_RESULT_SCHEMA,
  runBoundedProductionLoop,
};
