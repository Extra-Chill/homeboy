#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
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
      legacyRunResponse: 'canonical/agent-task-run/v1',
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
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-runtime-contract-source.js'));
const {
  WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA,
  WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
  WP_CODEBOX_RUNTIME_PROFILE_SCHEMA,
  wpCodeboxProviderRuntimeInvocationContract,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

assert.equal(REQUIRED_RUNTIME_CONTRACT_PATHS.includes('schemas.runtimeBoundary.profile'), true);
assert.deepEqual(runtimeContractManifest(), canonicalManifest);
assert.deepEqual(providerRuntimeInvocationContract(), canonicalManifest.providerRuntime);

const schemas = runtimeContractSchemas();
assert.equal(WP_CODEBOX_RUNTIME_PROFILE_SCHEMA, schemas.runtimeBoundary.profile);
assert.equal(WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA, schemas.artifact.resultEnvelope);
assert.equal(WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA, schemas.agentTask.runResult);
assert.equal(WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS.workspace_capture, schemas.runnerWorkspace.captureResult);
assert.deepEqual(wpCodeboxProviderRuntimeInvocationContract(), {
  ...canonicalManifest.providerRuntime,
  result_schemas: {
    ...canonicalManifest.providerRuntime.result_schemas,
    artifact_result_envelope: schemas.artifact.resultEnvelope,
  },
});

validateCanonicalRuntimeContractManifest(canonicalManifest);

const runtimeContractSource = fs.readFileSync(
  path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-runtime-contract-source.js'),
  'utf8'
);
assert.deepEqual(
  [...runtimeContractSource.matchAll(/const\s+(FALLBACK_[A-Z0-9_]+)/g)].map((match) => match[1]),
  [],
  'Do not carry hardcoded WP Codebox fallback contract constants; consume the public Codebox runtime contract manifest instead.'
);
assert.doesNotMatch(runtimeContractSource, /homeboy-extensions-fallback/);
assert.doesNotMatch(runtimeContractSource, /\.cache\/homeboy\/wp-codebox|setupCacheCoreModuleCandidates|HOMEBOY_WP_CODEBOX_INSTALL_DIR/);

assert.throws(
  () => validateCanonicalRuntimeContractManifest({
    ...canonicalManifest,
    schemas: {
      ...canonicalManifest.schemas,
      runtimeBoundary: {
        ...canonicalManifest.schemas.runtimeBoundary,
        profile: undefined,
      },
    },
  }),
  /missing schemas\.runtimeBoundary\.profile/
);

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

await assert.rejects(
  () => loadRuntimeContractSource({ wpCodeboxCoreModule: path.join(tempRoot, 'missing-runtime-core.mjs') }),
  /canonical runtime contract manifest is unavailable/
);

assert.throws(
  () => loadCanonicalRuntimeContractSourceSync({ wpCodeboxCoreModule: path.join(tempRoot, 'missing-runtime-core.cjs'), required: true }),
  /canonical runtime contract manifest is unavailable/
);

delete process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
const contractCandidates = coreModuleCandidates({ wpCodeboxInstallDir: path.join(tempRoot, 'wp-codebox-install') });
assert.equal(contractCandidates[0], '@automattic/wp-codebox-core/contracts');
assert.equal(contractCandidates[1], 'wp-codebox-workspace/contracts');
assert.equal(contractCandidates.length, 2);

const mismatchedModule = path.join(tempRoot, 'mismatched-runtime-core.mjs');
fs.writeFileSync(mismatchedModule, `
export function runtimeContractManifest() {
  const manifest = ${JSON.stringify(canonicalManifest, null, 2)};
  delete manifest.providerRuntime.tasks.workspaceCommand;
  return manifest;
}
`);

await assert.rejects(
  () => loadCanonicalRuntimeContractSource({ wpCodeboxCoreModule: mismatchedModule, required: true }),
  (error) => {
    assert.match(error.message, /canonical runtime contract manifest is unavailable/);
    assert.match(error.wpCodeboxRuntimeContractErrors[0].message, /providerRuntime\.tasks\.workspaceCommand/);
    return true;
  }
);

console.log('wp-codebox runtime contract source smoke passed');
