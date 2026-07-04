'use strict';

const assert = require('node:assert/strict');

const fuzzReadinessLevels = new Set(['declared', 'executable', 'proven']);
const fuzzCaseIntentSchema = 'homeboy/fuzz-workload-intent/v1';
const fullSurfaceCoverageTypes = new Set(['rest', 'admin', 'frontend', 'browser', 'database']);
const fullSurfaceGapReportFields = new Set(['surface_type', 'expected', 'covered', 'gaps', 'status', 'evidence_refs']);

function assertGenericFuzzManifest(manifest, {
  file,
  declaredIds,
  benchWorkloadIds = new Set(),
  benchProfileIds = new Set(),
  targetType = 'wordpress-plugin',
  targetSlug,
  workloadTypes = ['php', 'json'],
  requireCaseSafetyClass = false,
  requireCaseArtifacts = true,
  requireExpectedArtifacts = true,
  requireExpectedArtifactSemanticKeys = false,
  requireReadinessMetadata = false,
  requireRunnerNeutralIntent = false,
} = {}) {
  assert.equal(manifest.schema, 'homeboy/fuzz-workload/v1', `${file} schema mismatch`);
  assert.equal(typeof manifest.id, 'string', `${file} requires id`);

  if (declaredIds) {
    assert.ok(declaredIds.has(manifest.id), `${manifest.id} is not declared in rig fuzz_workloads.wordpress`);
  }

  assert.ok(!benchWorkloadIds.has(manifest.id), `${manifest.id} must not appear in bench_workloads`);
  assert.ok(!benchProfileIds.has(manifest.id), `${manifest.id} must not appear in bench_profiles`);

  assert.equal(manifest.target?.type, targetType, `${manifest.id} target.type mismatch`);
  if (targetSlug) {
    assert.equal(manifest.target?.slug, targetSlug, `${manifest.id} target.slug mismatch`);
  }

  assert.equal(manifest.workload?.runner, 'wp-codebox', `${manifest.id} workload.runner mismatch`);
  assert.equal(manifest.workload?.path, manifest.metadata?.workload_path, `${manifest.id} workload path must match metadata`);
  assert.ok(workloadTypes.includes(manifest.workload?.type), `${manifest.id} workload.type must be ${workloadTypes.join(', or ')}`);
  assert.deepEqual(manifest.coverage?.surface_ids, manifest.surface_ids, `${manifest.id} coverage surface ids drifted`);
  assert.deepEqual(manifest.coverage?.operations, manifest.operations, `${manifest.id} coverage operations drifted`);
  assert.equal(manifest.limits?.max_cases, manifest.case_budget, `${manifest.id} max_cases must match case_budget`);
  assert.equal(manifest.limits?.max_duration_seconds, manifest.duration_budget_seconds, `${manifest.id} max_duration_seconds must match duration_budget_seconds`);

  if (requireReadinessMetadata) {
    assert.ok(manifest.metadata?.readiness, `${manifest.id} requires metadata.readiness`);
  }

  if (manifest.metadata?.readiness) {
    assertBasicFuzzReadinessMetadata(manifest.metadata.readiness, { file });
  }

  assert.equal(manifest.cases?.length, 1, `${manifest.id} requires one default runner case`);
  const [runnerCase] = manifest.cases;
  assert.equal(runnerCase.case_id, `${manifest.id}:default`, `${manifest.id} default case id mismatch`);
  if (requireCaseSafetyClass) {
    assert.equal(runnerCase.metadata?.safety_class, manifest.safety_class, `${manifest.id} case safety class must match workload safety class`);
  }
  assert.deepEqual(runnerCase.surface_ids, manifest.surface_ids, `${manifest.id} case surface ids drifted`);
  assert.deepEqual(runnerCase.operations, manifest.operations, `${manifest.id} case operations drifted`);
  if (runnerCase.intent) {
    assert.equal(runnerCase.intent.schema, fuzzCaseIntentSchema, `${manifest.id} case intent schema mismatch`);
  } else {
    assert.equal(requireRunnerNeutralIntent, false, `${manifest.id} requires runner-neutral case intent`);
    assert.ok(Array.isArray(runnerCase.phases?.action), `${manifest.id} requires action phase`);
    assert.ok(runnerCase.phases.action.length > 0, `${manifest.id} requires at least one action step`);
  }
  assert.ok(Array.isArray(runnerCase.artifacts), `${manifest.id} requires case artifacts`);
  assert.ok(Array.isArray(manifest.artifacts?.expected), `${manifest.id} requires expected artifacts`);

  if (requireCaseArtifacts) {
    for (const artifact of runnerCase.artifacts) {
      assert.equal(artifact.required, true, `${manifest.id} case artifact ${artifact.name} must be required`);
    }
  }

  for (const artifact of manifest.artifacts.expected) {
    if (requireExpectedArtifacts) {
      assert.equal(artifact.required, true, `${manifest.id} expected artifact ${artifact.name} must be required`);
    }
    if (requireExpectedArtifactSemanticKeys) {
      assert.equal(typeof artifact.semantic_key, 'string', `${manifest.id} expected artifact ${artifact.name} requires semantic_key`);
    }
  }

  return runnerCase;
}

function assertRunnerNeutralFuzzCaseIntent(manifest, runnerCase) {
  assert.equal(runnerCase?.intent?.schema, fuzzCaseIntentSchema, `${manifest.id} case intent schema mismatch`);
}

function assertBasicFuzzReadinessMetadata(readiness, { file = 'fuzz workload' } = {}) {
  assert.ok(readiness && typeof readiness === 'object' && !Array.isArray(readiness), `${file} metadata.readiness must be an object`);
  assert.ok(fuzzReadinessLevels.has(readiness.level), `${file} metadata.readiness.level must be declared, executable, or proven`);
  if (readiness.level !== 'declared') {
    assert.equal(typeof readiness.coverage_contract, 'string', `${file} metadata.readiness.coverage_contract must describe the contract`);
    assert.notEqual(readiness.coverage_contract.trim(), '', `${file} metadata.readiness.coverage_contract must not be empty`);
  }
}

function assertGenericArtifactPostprocessWorkloadContract(workload, { id, helper, action, artifact, outputPath, schema, runnerSupportStatus = 'supported', readinessLevel = 'executable' }) {
  assert.equal(workload.schema, 'wp-codebox/wordpress-workload-run/v1', `${id} workload must use the generic workload-run contract`);
  assert.equal(workload.id, id, `${id} workload id drifted`);
  assert.equal(workload.steps?.length, 1, `${id} must declare one artifact postprocess step`);
  assert.equal(workload.steps[0].command, 'homeboy.artifact-postprocess', `${id} must use the generic artifact postprocess command`);
  assert.equal(workload.steps[0].type, undefined, `${id} must not invent an unsupported step type`);
  assert.equal(workload.steps[0].runner_support_status, undefined, `${id} runner support status belongs in metadata, not the executable step`);

  const args = workload.steps[0].args;
  assert.equal(typeof args.helper, 'string', `${id} helper must be a package-relative helper path`);
  assert.ok(args.helper.startsWith('${package.root}/'), `${id} helper must be package-relative`);
  assert.equal(args.helper, helper, `${id} helper drifted`);
  assert.equal(args.action, action, `${id} action drifted`);
  assert.deepEqual(args.input, {
    type: 'artifact-root',
    path: '${artifacts.root}',
    artifact_globs: ['**/*.json'],
    max_bytes: 1048576,
  }, `${id} input artifact binding drifted`);
  assert.equal(args.output.artifact, artifact, `${id} output artifact drifted`);
  assert.equal(args.output.path, outputPath, `${id} output path drifted`);
  assert.equal(args.output.kind, 'json', `${id} output kind drifted`);
  assert.equal(args.output.contentType, 'application/json', `${id} output contentType drifted`);
  assert.equal(args.output.schema, schema, `${id} output schema drifted`);
  assert.equal(args.output.semantic_key, 'fuzz.report', `${id} semantic key drifted`);

  assert.deepEqual(workload.artifacts?.[0], {
    name: artifact,
    path: outputPath,
    kind: 'json',
    contentType: 'application/json',
    required: true,
    metadata: {
      schema,
      semantic_key: 'fuzz.report',
    },
  }, `${id} collected artifact declaration drifted`);

  assert.equal(workload.metadata?.runner_support_status, runnerSupportStatus, `${id} artifact-postprocess runner support status drifted`);
  assert.equal(workload.metadata?.readiness?.level, readinessLevel, `${id} artifact-postprocess readiness level drifted`);
  assert.ok(workload.metadata?.readiness?.proven_when?.some((condition) => condition.includes('artifact root')), `${id} readiness must describe the artifact-root proof condition`);
  assert.ok(workload.metadata?.readiness?.proven_when?.some((condition) => condition.includes('reviewer-facing evidence')), `${id} readiness must describe the reviewer-facing artifact proof condition`);
  assert.equal(workload.metadata?.missing_upstream_contract, undefined, `${id} must not claim missing upstream artifact-postprocess fields`);
}

function assertFullSurfaceCoverageManifest(manifest, { file = manifest.property } = {}) {
  assert.equal(manifest.schema, 'homeboy-rigs/wordpress-full-surface-coverage/v1', `${file} schema mismatch`);
  assert.equal(typeof manifest.property, 'string', `${file} requires property`);
  assert.notEqual(manifest.property.trim(), '', `${file} requires non-empty property`);
  assert.ok(manifest.coverage_map && typeof manifest.coverage_map === 'object' && !Array.isArray(manifest.coverage_map), `${file} requires coverage_map`);

  const declaredSurfaceTypes = new Set(fullSurfaceCoverageTypes);
  if (!manifest.coverage_map.browser) {
    declaredSurfaceTypes.delete('browser');
  }

  for (const surfaceType of declaredSurfaceTypes) {
    const entry = manifest.coverage_map[surfaceType];
    assert.ok(entry && typeof entry === 'object' && !Array.isArray(entry), `${file} coverage_map.${surfaceType} must be an object`);
    assert.equal(entry.surface_type, surfaceType, `${file} coverage_map.${surfaceType}.surface_type mismatch`);
    assert.equal(typeof entry.surface_id, 'string', `${file} coverage_map.${surfaceType}.surface_id must be a string`);
    assert.notEqual(entry.surface_id.trim(), '', `${file} coverage_map.${surfaceType}.surface_id must be non-empty`);
    assert.equal(typeof entry.coverage_goal, 'string', `${file} coverage_map.${surfaceType}.coverage_goal must be a string`);
    assert.notEqual(entry.coverage_goal.trim(), '', `${file} coverage_map.${surfaceType}.coverage_goal must be non-empty`);
    assertStringArray(entry.workload_ids, `${file} coverage_map.${surfaceType}.workload_ids`);
    assertStringArray(entry.artifact_schemas, `${file} coverage_map.${surfaceType}.artifact_schemas`);
  }

  assert.ok(manifest.gap_report && typeof manifest.gap_report === 'object' && !Array.isArray(manifest.gap_report), `${file} requires gap_report`);
  assert.equal(manifest.gap_report.schema, 'homeboy-rigs/wordpress-coverage-gap-report/v1', `${file} gap_report.schema mismatch`);
  assertStringArray(manifest.gap_report.inputs, `${file} gap_report.inputs`);
  assertStringArray(manifest.gap_report.required_fields, `${file} gap_report.required_fields`);
  assert.equal(manifest.gap_report.semantic_key, 'fuzz.report', `${file} gap_report.semantic_key mismatch`);
  assert.ok(manifest.gap_report.compare && typeof manifest.gap_report.compare === 'object' && !Array.isArray(manifest.gap_report.compare), `${file} gap_report.compare must be an object`);

  const requiredFields = new Set(manifest.gap_report.required_fields);
  for (const field of fullSurfaceGapReportFields) {
    assert.ok(requiredFields.has(field), `${file} gap_report.required_fields missing ${field}`);
  }

  for (const surfaceType of declaredSurfaceTypes) {
    assert.equal(typeof manifest.gap_report.compare[surfaceType], 'string', `${file} gap_report.compare.${surfaceType} must be a string`);
    assert.notEqual(manifest.gap_report.compare[surfaceType].trim(), '', `${file} gap_report.compare.${surfaceType} must be non-empty`);
  }
}

function assertStringArray(value, label) {
  assert.ok(Array.isArray(value), `${label} must be an array`);
  assert.ok(value.length > 0, `${label} must not be empty`);
  for (const entry of value) {
    assert.equal(typeof entry, 'string', `${label} entries must be strings`);
    assert.notEqual(entry.trim(), '', `${label} entries must be non-empty`);
  }
}

module.exports = {
  fuzzCaseIntentSchema,
  fuzzReadinessLevels,
  fullSurfaceCoverageTypes,
  fullSurfaceGapReportFields,
  assertFullSurfaceCoverageManifest,
  assertGenericArtifactPostprocessWorkloadContract,
  assertGenericFuzzManifest,
  assertRunnerNeutralFuzzCaseIntent,
};
