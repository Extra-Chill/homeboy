'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  ARTIFACT_POSTPROCESS_CONTRACT,
  DEFAULT_FUZZ_SUITE_ABILITY,
  DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY,
  DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
  WP_CODEBOX_FUZZ_SUITE_SCHEMA,
  normalizeWpCodeboxFuzzSuiteResult,
  wpCodeboxFuzzSuiteAbility,
  wpCodeboxFuzzSuiteInput,
  wpCodeboxFuzzSuiteSchema,
  wpCodeboxRuntimeContractManifest,
  wpCodeboxWordPressWorkloadRunAbility,
  wpCodeboxWordPressWorkloadRunInput,
  wpCodeboxWordPressWorkloadRunSchema,
} = require('../lib/wp-codebox-fuzz-run');

function exportedHomeboyContract(contractId) {
  const exportRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-contract-export-'));
  try {
    const result = spawnSync('homeboy', ['contract', 'export', '--dir', exportRoot], { encoding: 'utf8' });
    if (result.status !== 0) {
      return undefined;
    }
    const catalogPath = path.join(exportRoot, 'schema-catalog.json');
    if (!fs.existsSync(catalogPath)) {
      return undefined;
    }
    const catalog = JSON.parse(fs.readFileSync(catalogPath, 'utf8'));
    return (Array.isArray(catalog.contracts) ? catalog.contracts : []).find((contract) => contract.id === contractId);
  } finally {
    fs.rmSync(exportRoot, { recursive: true, force: true });
  }
}

const input = wpCodeboxFuzzSuiteInput({
  id: 'fuzz-smoke',
  target: { type: 'wordpress-plugin', slug: 'sample-plugin' },
  workload: { entry: 'rest-routes' },
  cases: [{ method: 'GET', path: '/wp/v2/posts' }],
  limits: { max_cases: 1 },
  metadata: { scenario: 'smoke' },
  runtimeContractManifest: { schemas: { wordpressRuntime: { fuzzSuite: WP_CODEBOX_FUZZ_SUITE_SCHEMA } } },
});
assert.equal(input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(input.target.slug, 'sample-plugin');
assert.deepEqual(input.metadata.limits, { max_cases: 1 });

const manifest = {
  schema: 'wp-codebox/runtime-contract-manifest/v1',
  version: 1,
  schemas: {
    wordpressRuntime: {
      workloadRun: 'wp-codebox/wordpress-workload-run/v1',
      fuzzSuite: 'wp-codebox/fuzz-suite/v1',
      fuzzSuiteResult: 'wp-codebox/fuzz-suite-result/v1',
    },
  },
  abilities: {
    wordpressRuntime: {
      runWorkload: 'wp-codebox/run-wordpress-workload',
      runFuzzSuite: 'wp-codebox/run-fuzz-suite',
    },
  },
  commands: { wordpressRuntime: { runWorkload: 'run-wordpress-workload', runFuzzSuite: 'run-fuzz-suite' } },
  readiness: { wordpressRuntime: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready' } },
};

assert.equal(wpCodeboxFuzzSuiteAbility({ runtimeContractManifest: manifest }), DEFAULT_FUZZ_SUITE_ABILITY);
assert.equal(wpCodeboxFuzzSuiteSchema({ runtimeContractManifest: manifest }), WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(wpCodeboxWordPressWorkloadRunAbility({ runtimeContractManifest: manifest }), DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY);
assert.equal(wpCodeboxWordPressWorkloadRunSchema({ runtimeContractManifest: manifest }), DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA);
assert.equal(wpCodeboxRuntimeContractManifest({ runtimeContractManifest: manifest }), manifest);
assert.deepEqual(wpCodeboxRuntimeContractManifest(), {});

const artifactPostprocessWorkloadInput = wpCodeboxWordPressWorkloadRunInput({
  id: 'artifact-postprocess-workload-run',
  steps: [{
    command: 'artifact-postprocess',
    args: {
      helper: '${package.root}/tools/artifact-helper.mjs',
      action: 'coverage-gap-report',
      input: { type: 'artifact-root', path: '${artifacts.root}', max_bytes: 1024 },
      output: { artifact: 'coverage_gap_report', path: 'coverage/gaps.json', kind: 'json', schema: 'homeboy-rigs/wordpress-coverage-gap-report/v1', semantic_key: 'fuzz.coverage.gap_report' },
      parameters: { max_bytes: 1024 },
    },
  }],
});
assert.equal(artifactPostprocessWorkloadInput.steps[0].type, 'artifact-postprocess');
assert.equal(artifactPostprocessWorkloadInput.steps[0].metadata.contract, ARTIFACT_POSTPROCESS_CONTRACT);
assert.deepEqual(artifactPostprocessWorkloadInput.steps[0].args, ['coverage-gap-report', '${inputArtifactRoot}', '${outputArtifactPath}', JSON.stringify({ max_bytes: 1024 })]);

const artifactPostprocessHomeboyContract = exportedHomeboyContract(ARTIFACT_POSTPROCESS_CONTRACT);
if (artifactPostprocessHomeboyContract) {
  assert.equal(artifactPostprocessHomeboyContract.id, ARTIFACT_POSTPROCESS_CONTRACT);
  assert.equal(artifactPostprocessHomeboyContract.version, 1);
}

const normalizedResult = normalizeWpCodeboxFuzzSuiteResult({
  success: true,
  status: 'completed',
  summary: { status: 'passed' },
  artifacts: [{ name: 'case_log', kind: 'json', path: 'case-log.json', schema: 'homeboy/wordpress-fuzz-case-log/v1' }],
  observations: [{ id: 'rest-posts', status: 'passed', metrics: { response_ms: 12 } }],
}, {
  request: {
    schema: 'homeboy/wp-codebox-fuzz-execution/v1',
    task_id: 'fuzz-smoke',
    ability: 'wp-codebox/run-fuzz-suite',
  },
});
assert.equal(normalizedResult.status, 'completed');
assert.equal(normalizedResult.runtime_task_result, undefined);
assert.equal(normalizedResult.metadata.direct_execution_request.schema, 'homeboy/wp-codebox-fuzz-execution/v1');
