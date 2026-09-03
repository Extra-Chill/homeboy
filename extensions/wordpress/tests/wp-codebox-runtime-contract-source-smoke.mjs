#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const wordpressDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-contract-'));

const canonicalManifest = {
  schema: 'wp-codebox/runtime-contract-manifest/v1',
  version: 1,
  schemas: {
    providerRuntime: {
      invocation: 'canonical/provider-runtime-invocation-contract/v1',
      credentialRequirements: 'canonical/provider-credential-requirements/v1',
      credentialPreflight: 'canonical/provider-credential-preflight/v1',
      credentialResolution: 'canonical/provider-credential-resolution/v1',
    },
    agentTask: {
      runRequest: 'canonical/run-agent-task/v1',
      runResult: 'canonical/agent-task-run-result/v1',
    },
    runtimeBoundary: {
      profile: 'canonical/runtime-profile/v1',
      previewLease: 'canonical/preview-lease/v1',
      browserContainedSiteStatus: 'canonical/browser-contained-site-status/v1',
      browserContainedSiteOpen: 'canonical/browser-contained-site-open/v1',
      browserSessionProductDto: 'canonical/browser-session-product-dto/v1',
      browserPreviewBootConfig: 'canonical/browser-preview-boot-config/v1',
    },
    artifact: {
      resultEnvelope: 'canonical/artifact-result-envelope/v1',
    },
    runnerWorkspace: {
      prepareRequest: 'canonical/runner-workspace-prepare-request/v1',
      prepareResult: 'canonical/runner-workspace-prepare-result/v1',
      captureRequest: 'canonical/runner-workspace-capture-request/v1',
      captureResult: 'canonical/runner-workspace-capture-result/v1',
      commandRequest: 'canonical/runner-workspace-command-request/v1',
      commandResult: 'canonical/runner-workspace-command-result/v1',
      publicationRequest: 'canonical/runner-workspace-publication-request/v1',
      publicationResult: 'canonical/runner-workspace-publication-result/v1',
    },
    fanoutAggregation: {
      input: 'canonical/fanout-aggregation-input/v1',
      output: 'canonical/fanout-aggregation-output/v1',
    },
    wordpressRuntime: {
      fuzzSuite: 'canonical/fuzz-suite/v1',
      fuzzSuiteResult: 'canonical/fuzz-suite-result/v1',
      workloadRun: 'canonical/wordpress-workload-run/v1',
    },
  },
  abilities: {
    wordpressRuntime: {
      runFuzzSuite: 'wp-codebox/run-fuzz-suite',
      runWorkload: 'wp-codebox/run-wordpress-workload',
    },
  },
  commands: {
    wordpressRuntime: {
      runFuzzSuite: 'run-fuzz-suite',
      runWorkload: 'run-wordpress-workload',
    },
  },
  capabilities: {
    wordpressRuntime: {
      commands: {
        runFuzzSuite: 'run-fuzz-suite',
        runWorkload: 'run-wordpress-workload',
      },
    },
  },
  readiness: {
    wordpressRuntime: {
      schema: 'wp-codebox/fuzz-runner-readiness/v1',
      entrypoint: 'run-fuzz-suite --runner-mode=runtime-backed',
      mode: 'runtime-backed',
    },
  },
  providerRuntime: {
    schema: 'canonical/provider-runtime-invocation-contract/v1',
    version: 1,
    tasks: {
      workspacePrepare: 'canonical.runner-workspace.prepare',
      workspaceCapture: 'canonical.runner-workspace.capture',
      workspaceCommand: 'canonical.runner-workspace.command',
      workspacePublish: 'canonical.runner-workspace.publish',
      toolCallTranscriptRecord: 'canonical.tool-call-transcript.record',
      artifactHandoff: 'canonical.artifact-handoff',
    },
    abilities: {
      workspacePrepare: 'canonical/runner-workspace-prepare',
      workspaceCapture: 'canonical/runner-workspace-capture',
      workspaceCommand: 'canonical/runner-workspace-command',
      workspacePublish: 'canonical/runner-workspace-publish',
      toolCallTranscriptRecord: 'canonical/record-tool-call-transcript',
      artifactHandoff: 'canonical/handoff-artifacts',
    },
    result_schemas: {
      workspace_prepare: 'canonical/runner-workspace-prepare-result/v1',
      workspace_capture: 'canonical/runner-workspace-capture-result/v1',
      workspace_command: 'canonical/runner-workspace-command-result/v1',
      workspace_publication: 'canonical/runner-workspace-publication-result/v1',
      tool_call_transcript: 'canonical/tool-call-transcript/v1',
      evidence_artifact_envelope: 'canonical/evidence-artifact-envelope/v1',
      artifact_result_envelope: 'canonical/artifact-result-envelope/v1',
    },
  },
};

const canonicalModule = path.join(tempRoot, 'canonical-runtime-core.cjs');
fs.writeFileSync(canonicalModule, `
module.exports = {
  runtimeContractManifest() {
    return ${JSON.stringify(canonicalManifest, null, 2)};
  },
  RUNTIME_CONTRACT_NORMALIZERS: {
    runtimeProfile(input) { return { ...input, schema: 'canonical/runtime-profile/v1' }; },
  },
};
`);
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE = canonicalModule;

const {
  REQUIRED_RUNTIME_CONTRACT_PATHS,
  coreModuleCandidates,
  loadCanonicalRuntimeContractSource,
  loadCanonicalRuntimeContractSourceSync,
  loadRuntimeContractSource,
  providerRuntimeInvocationContract,
  runtimeContractManifest,
  runtimeContractSchemas,
  validateCanonicalRuntimeContractManifest,
} = require(path.join(wordpressDir, 'lib', 'wp-codebox-runtime-contract-source.js'));
const { WP_CODEBOX_RUNTIME_PROFILE_SCHEMA } = require(path.join(wordpressDir, 'lib', 'wp-codebox-runtime-profile.js'));
const { artifactResultEnvelopeSchema } = require(path.join(wordpressDir, 'lib', 'wp-codebox-artifact-contract.js'));

for (const requiredPath of REQUIRED_RUNTIME_CONTRACT_PATHS) {
  assert.equal(
    typeof requiredPath.split('.').reduce((value, key) => (value == null ? value : value[key]), canonicalManifest),
    'string',
    `Required runtime contract path ${requiredPath} must resolve in the canonical manifest`
  );
}
assert.deepEqual(runtimeContractManifest(), canonicalManifest);
assert.deepEqual(providerRuntimeInvocationContract(), canonicalManifest.providerRuntime);

const schemas = runtimeContractSchemas();
assert.equal(WP_CODEBOX_RUNTIME_PROFILE_SCHEMA, schemas.runtimeBoundary.profile);
assert.equal(artifactResultEnvelopeSchema(), schemas.artifact.resultEnvelope);

validateCanonicalRuntimeContractManifest(canonicalManifest);

const canonical = await loadCanonicalRuntimeContractSource({ wpCodeboxCoreModule: canonicalModule, required: true });
assert.equal(canonical.canonical, true);
assert.equal(canonical.source, pathToFileURL(canonicalModule).href);
assert.deepEqual(canonical.manifest, canonicalManifest);
assert.equal(typeof canonical.normalizers.runtimeProfile, 'function');

const canonicalSync = loadCanonicalRuntimeContractSourceSync({ wpCodeboxCoreModule: canonicalModule, required: true });
assert.equal(canonicalSync.canonical, true);
assert.deepEqual(canonicalSync.manifest, canonicalManifest);

const loaded = await loadRuntimeContractSource({ wpCodeboxCoreModule: canonicalModule });
assert.equal(loaded.canonical, true);
assert.deepEqual(loaded.manifest, canonicalManifest);

delete process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
const contractCandidates = coreModuleCandidates({ wpCodeboxInstallDir: path.join(tempRoot, 'wp-codebox-install') });
assert.equal(contractCandidates[0], pathToFileURL(path.join(tempRoot, 'wp-codebox-install/source/node_modules/@automattic/wp-codebox-core/dist/contracts.js')).href);
assert.equal(contractCandidates[1], pathToFileURL(path.join(tempRoot, 'wp-codebox-install/release/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist/contracts.js')).href);
assert.equal(contractCandidates[2], '@automattic/wp-codebox-core/contracts');
assert.equal(contractCandidates[3], 'wp-codebox-workspace/contracts');
assert.equal(contractCandidates.length, 4);

console.log('wp-codebox runtime contract source smoke passed');
