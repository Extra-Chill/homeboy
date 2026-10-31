'use strict';

const assert = require('node:assert/strict');

const {
  assertFullSurfaceCoverageManifest,
  assertGenericArtifactPostprocessWorkloadContract,
  assertGenericFuzzManifest,
} = require('../lib/fuzz-manifest-contracts');

const workload = {
  schema: 'homeboy/fuzz-workload/v1',
  id: 'product-api',
  target: { type: 'wordpress-plugin', slug: 'woocommerce' },
  surface_ids: ['product-api'],
  operations: ['read'],
  case_budget: 1,
  duration_budget_seconds: 10,
  limits: { max_cases: 1, max_duration_seconds: 10 },
  workload: { runner: 'wp-codebox', type: 'json', path: '${package.root}/fuzz/product-api.json' },
  coverage: { surface_ids: ['product-api'], operations: ['read'] },
  metadata: {
    workload_path: '${package.root}/fuzz/product-api.json',
    readiness: { level: 'executable', coverage_contract: 'Emits reviewer-facing artifacts.' },
  },
  cases: [
    {
      case_id: 'product-api:default',
      surface_ids: ['product-api'],
      operations: ['read'],
      phases: { action: [{ type: 'noop' }] },
      artifacts: [{ name: 'raw_result', required: true }],
    },
  ],
  artifacts: { expected: [{ name: 'raw_result', required: true, semantic_key: 'fuzz.raw' }] },
};

const runnerCase = assertGenericFuzzManifest(workload, {
  file: 'product-api.json',
  declaredIds: new Set(['product-api']),
  targetSlug: 'woocommerce',
  requireExpectedArtifactSemanticKeys: true,
});
assert.equal(runnerCase.case_id, 'product-api:default');

assertGenericArtifactPostprocessWorkloadContract({
  schema: 'wp-codebox/wordpress-workload-run/v1',
  id: 'coverage-gap-report',
  steps: [{ command: 'homeboy.artifact-postprocess', args: {
    helper: '${package.root}/tools/gap-report.mjs',
    action: 'summarize',
    input: { type: 'artifact-root', path: '${artifacts.root}', artifact_globs: ['**/*.json'], max_bytes: 1048576 },
    output: { artifact: 'coverage_gap_report', path: 'coverage-gap-report.json', kind: 'json', contentType: 'application/json', schema: 'homeboy-rigs/wordpress-coverage-gap-report/v1', semantic_key: 'fuzz.report' },
  } }],
  artifacts: [{ name: 'coverage_gap_report', path: 'coverage-gap-report.json', kind: 'json', contentType: 'application/json', required: true, metadata: { schema: 'homeboy-rigs/wordpress-coverage-gap-report/v1', semantic_key: 'fuzz.report' } }],
  metadata: { runner_support_status: 'supported', readiness: { level: 'executable', proven_when: ['artifact root contains inputs', 'reviewer-facing evidence is attached'] } },
}, {
  id: 'coverage-gap-report',
  helper: '${package.root}/tools/gap-report.mjs',
  action: 'summarize',
  artifact: 'coverage_gap_report',
  outputPath: 'coverage-gap-report.json',
  schema: 'homeboy-rigs/wordpress-coverage-gap-report/v1',
});

assertFullSurfaceCoverageManifest({
  schema: 'homeboy-rigs/wordpress-full-surface-coverage/v1',
  property: 'woocommerce',
  coverage_map: {
    rest: { surface_type: 'rest', surface_id: 'rest', coverage_goal: 'cover REST', workload_ids: ['product-api'], artifact_schemas: ['schema'] },
    admin: { surface_type: 'admin', surface_id: 'admin', coverage_goal: 'cover admin', workload_ids: ['product-api'], artifact_schemas: ['schema'] },
    frontend: { surface_type: 'frontend', surface_id: 'frontend', coverage_goal: 'cover frontend', workload_ids: ['product-api'], artifact_schemas: ['schema'] },
    database: { surface_type: 'database', surface_id: 'database', coverage_goal: 'cover database', workload_ids: ['product-api'], artifact_schemas: ['schema'] },
  },
  gap_report: {
    schema: 'homeboy-rigs/wordpress-coverage-gap-report/v1',
    inputs: ['coverage'],
    required_fields: ['surface_type', 'expected', 'covered', 'gaps', 'status', 'evidence_refs'],
    semantic_key: 'fuzz.report',
    compare: { rest: 'REST', admin: 'admin', frontend: 'frontend', database: 'database' },
  },
});

console.log('fuzz manifest contracts smoke passed');
