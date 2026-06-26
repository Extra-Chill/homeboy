'use strict';

const {
  buildControllerLoopProofSpec,
  controllerProofPolicy,
  runGenericAgentLoop,
} = require('./generic-agent-loop-runner');
const { runBoundedProductionLoop } = require('./bounded-production-loop-runner');
const { validateControllerLoopProof } = require('./controller-loop-proof-validator');
const { evaluateGatePlan, evaluateGateResults } = require('./gate-plan-evaluator');
const {
  recordLifecycle,
  runDeterministicWorkspaceLifecycle,
  scenarioById,
} = require('./workspace-publication-lifecycle.cjs');

function runAgentTaskToReview(options = {}) {
  const plan = requiredObject(options.plan, 'plan');
  const runtime = requiredObject(options.runtime, 'runtime');
  const workspace = options.workspace || plan.component_path || plan.workspace;
  if (!workspace) {
    throw new Error('runAgentTaskToReview requires options.workspace or plan.component_path.');
  }

  const runtimeResult = runGenericAgentLoop({
    ...options,
    plan,
    runtime,
  });
  const results = ensureResults(runtimeResult.results, runtimeResult, plan);
  const scenario = ensureScenario(results, runtimeResult, plan);
  const lifecycle = runDeterministicWorkspaceLifecycle(
    { ...plan, ...optionalObject(options.lifecycle), ...optionalObject(options.publication) },
    results,
    scenario,
    workspace,
    options.hooks || {},
  );
  recordLifecycle(results, scenario, lifecycle);

  const runtimeGate = evaluateGatePlan({
    id: 'runtime_agent_task',
    label: 'Runtime Agent Task',
    pass_when: [{ field: 'succeeded', op: 'truthy', reason: 'runtime_agent_task_failed' }],
  }, { succeeded: runtimeSucceeded(runtimeResult) });
  const lifecycleGate = lifecycle.gateSummary || evaluateGateResults([]);
  const publicationGate = evaluatePublicationGate(lifecycle.publication, lifecycle.capture, plan);
  const productionProof = buildReviewProductionProof({ runtimeResult, lifecycle, plan, validationPolicy: options.validationPolicy || options.validation_policy || {} });
  const controllerProofValidation = validateControllerLoopProof({
    spec: buildControllerLoopProofSpec({ request: runtimeResult.request, plan, validationPolicy: options.validationPolicy || options.validation_policy || {} }),
    proof: productionProof,
    policy: controllerProofPolicy(options.validationPolicy || options.validation_policy || {}, plan),
  });
  attachReviewProofValidation(results, { productionProof, controllerProofValidation });
  const proofGate = evaluateGatePlan({
    id: 'production_proof',
    label: 'Production Proof',
    pass_when: [{ field: 'valid', op: 'truthy', reason: 'production_proof_invalid' }],
  }, { valid: controllerProofValidation.valid });
  const finalGate = evaluateGateResults([runtimeGate, lifecycleGate, publicationGate, proofGate]);
  const status = finalGate.success && lifecycle.success ? 'succeeded' : 'failed';

  return {
    schema: 'homeboy/agent-task-to-review-result/v1',
    status,
    terminal_status: status === 'succeeded' ? 'green' : 'red',
    success: status === 'succeeded',
    task_id: runtimeResult.request?.task_id || plan.task_id || plan.workload_id || '',
    runtime_result: runtimeResult,
    verification_result: {
      verification: lifecycle.verification,
      drift: lifecycle.drift,
      side_effect_policy: lifecycle.sideEffectPolicy,
      writable_paths: lifecycle.writablePaths,
      workspace_contract: lifecycle.workspaceContract,
      gate_summary: lifecycleGate,
    },
    publication_result: lifecycle.publication,
    gate_result: finalGate,
    production_proof: productionProof,
    controller_proof_validation: controllerProofValidation,
    artifacts: collectArtifacts(runtimeResult, lifecycle),
    results,
    scenario,
    error: status === 'succeeded' ? '' : (lifecycle.error || finalGate.error || finalGate.reason || 'agent task to review failed'),
  };
}

function buildReviewProductionProof({ runtimeResult, lifecycle, plan, validationPolicy }) {
  const proofPolicy = controllerProofPolicy(validationPolicy, plan);
  const evidenceRefs = [
    ...normalizeArray(runtimeResult.outcome?.evidence_refs || runtimeResult.outcome?.evidence),
    ...normalizeArray(lifecycle.publication?.publication_evidence_ref ? [{ kind: 'publication', ...lifecycle.publication.publication_evidence_ref }] : []),
  ];
  const outcome = {
    ...runtimeResult.outcome,
    status: runtimeSucceeded(runtimeResult) && lifecycle.success ? 'succeeded' : 'failed',
    artifacts: normalizeArray(runtimeResult.outcome?.artifacts),
    evidence_refs: evidenceRefs,
  };
  return runBoundedProductionLoop({
    loopId: `${runtimeResult.request?.task_id || plan.task_id || plan.workload_id || 'agent-task'}-review-proof`,
    maxIterations: 1,
    executeIteration: () => outcome,
    acceptedStatuses: ['accepted', 'succeeded', 'passed', 'no_op'],
    artifactRequirements: normalizeArray(runtimeResult.request?.expected_artifacts).map((name) => ({ name })),
    evidenceRequirements: normalizeArray(proofPolicy.required_evidence || proofPolicy.evidence || proofPolicy.evidence_requirements),
    previewRequirement: proofPolicy.preview_required === true || proofPolicy.require_preview === true,
    publicationEvidenceRequirement: proofPolicy.publication_required === true || proofPolicy.require_publication === true || proofPolicy.pr_required === true || proofPolicy.require_pr === true,
  });
}

function attachReviewProofValidation(results, { productionProof, controllerProofValidation }) {
  if (!results || !Array.isArray(results.scenarios)) {
    return;
  }
  for (const scenario of results.scenarios) {
    scenario.metadata = optionalObject(scenario.metadata);
    scenario.metadata.controller_loop_proof = productionProof;
    scenario.metadata.controller_loop_proof_validation = controllerProofValidation;
  }
}

function evaluatePublicationGate(publication = {}, capture = {}, plan = {}) {
  const requiresReview = plan.success_requires_pr !== false && capture.changed;
  return evaluateGatePlan({
    id: 'publication_evidence',
    label: 'Publication Evidence',
    enabled: Boolean(requiresReview),
    pass_when: [{ field: 'has_evidence', op: 'truthy', reason: 'publication_evidence_missing' }],
  }, {
    has_evidence: Boolean(publication.opened || publication.dry_run || publication.url || publication.publication_evidence_ref?.head),
  });
}

function collectArtifacts(runtimeResult, lifecycle) {
  const runtimeArtifacts = Array.isArray(runtimeResult.loop?.artifacts) ? runtimeResult.loop.artifacts : [];
  const evidence = Array.isArray(runtimeResult.loop?.evidence) ? runtimeResult.loop.evidence : [];
  const runtimeEvidenceRefs = Array.isArray(runtimeResult.outcome?.evidence_refs) ? runtimeResult.outcome.evidence_refs : [];
  return {
    runtime_artifacts: runtimeArtifacts,
    runtime_evidence: evidence,
    runtime_evidence_refs: runtimeEvidenceRefs,
    changed_files: lifecycle.capture?.files || [],
    agent_files: lifecycle.capture?.agent_files || [],
    verification_side_effect_files: lifecycle.capture?.verification_side_effect_files || [],
    publication_evidence_ref: lifecycle.publication?.publication_evidence_ref || null,
  };
}

function runtimeSucceeded(runtimeResult) {
  return ['succeeded', 'no_op'].includes(runtimeResult.outcome?.status)
    && runtimeResult.assertion?.success !== false
    && runtimeResult.controllerProofValidation?.valid !== false;
}

function ensureResults(results, runtimeResult, plan) {
  if (results && typeof results === 'object') {
    results.scenarios = Array.isArray(results.scenarios) ? results.scenarios : [];
    return results;
  }
  return {
    schema: 'homeboy/runtime-agent-ci-results/v1',
    status: runtimeResult.outcome?.status === 'succeeded' ? 'completed' : 'failed',
    scenarios: [],
    metadata: { workload_id: plan.workload_id || plan.task_id || '' },
  };
}

function ensureScenario(results, runtimeResult, plan) {
  let scenario = scenarioById(results, plan.workload_id || plan.task_id || runtimeResult.request?.task_id || '');
  if (!scenario) {
    scenario = {
      id: runtimeResult.request?.task_id || plan.workload_id || plan.task_id || 'agent-task-to-review',
      status: runtimeResult.outcome?.status || 'unknown',
      metrics: {},
      metadata: {},
    };
    results.scenarios.push(scenario);
  }
  scenario.metrics = optionalObject(scenario.metrics);
  scenario.metadata = optionalObject(scenario.metadata);
  return scenario;
}

function requiredObject(value, name) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${name} is required.`);
  }
  return value;
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

module.exports = {
  runAgentTaskToReview,
};
