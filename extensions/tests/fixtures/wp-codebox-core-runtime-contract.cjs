'use strict';

const manifest = {
  schema: 'wp-codebox/runtime-contract-manifest/v1',
  version: 1,
  schemas: {
    providerRuntime: {
      invocation: 'wp-codebox/provider-runtime-invocation-contract/v1',
      credentialRequirements: 'wp-codebox/provider-credential-requirements/v1',
      credentialPreflight: 'wp-codebox/provider-credential-preflight/v1',
      credentialResolution: 'wp-codebox/provider-credential-resolution/v1',
    },
    agentTask: {
      runRequest: 'wp-codebox/run-agent-task/v1',
      runResult: 'wp-codebox/agent-task-run-result/v1',
      legacyRunResponse: 'wp-codebox/agent-task-run/v1',
    },
    runtimeBoundary: {
      profile: 'wp-codebox/runtime-profile/v1',
      previewLease: 'wp-codebox/preview-lease/v1',
      browserContainedSiteStatus: 'wp-codebox/browser-contained-site-status/v1',
      browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1',
      browserSessionProductDto: 'wp-codebox/browser-session-product-dto/v1',
      browserPreviewBootConfig: 'wp-codebox/browser-preview-boot-config/v1',
    },
    artifact: {
      resultEnvelope: 'wp-codebox/artifact-result-envelope/v1',
    },
    runnerWorkspace: {
      prepareRequest: 'wp-codebox/runner-workspace-prepare-request/v1',
      prepareResult: 'wp-codebox/runner-workspace-prepare-result/v1',
      captureRequest: 'wp-codebox/runner-workspace-capture-request/v1',
      captureResult: 'wp-codebox/runner-workspace-capture-result/v1',
      commandRequest: 'wp-codebox/runner-workspace-command-request/v1',
      commandResult: 'wp-codebox/runner-workspace-command-result/v1',
      publicationRequest: 'wp-codebox/runner-workspace-publication-request/v1',
      publicationResult: 'wp-codebox/runner-workspace-publication-result/v1',
    },
    fanoutAggregation: {
      input: 'wp-codebox/fanout-aggregation-input/v1',
      output: 'wp-codebox/fanout-aggregation-output/v1',
    },
  },
  providerRuntime: {
    schema: 'wp-codebox/provider-runtime-invocation-contract/v1',
    version: 1,
    tasks: {
      workspacePrepare: 'wp-codebox.runner-workspace.prepare',
      workspaceCapture: 'wp-codebox.runner-workspace.capture',
      workspaceCommand: 'wp-codebox.runner-workspace.command',
      workspacePublish: 'wp-codebox.runner-workspace.publish',
      toolCallTranscriptRecord: 'wp-codebox.tool-call-transcript.record',
      artifactHandoff: 'wp-codebox.artifact-handoff',
    },
    abilities: {
      workspacePrepare: 'wp-codebox/runner-workspace-prepare',
      workspaceCapture: 'wp-codebox/runner-workspace-capture',
      workspaceCommand: 'wp-codebox/runner-workspace-command',
      workspacePublish: 'wp-codebox/runner-workspace-publish',
      toolCallTranscriptRecord: 'wp-codebox/record-tool-call-transcript',
      artifactHandoff: 'wp-codebox/handoff-artifacts',
    },
    result_schemas: {
      workspace_prepare: 'wp-codebox/runner-workspace-prepare-result/v1',
      workspace_capture: 'wp-codebox/runner-workspace-capture-result/v1',
      workspace_command: 'wp-codebox/runner-workspace-command-result/v1',
      workspace_publication: 'wp-codebox/runner-workspace-publication-result/v1',
      tool_call_transcript: 'wp-codebox/tool-call-transcript/v1',
      evidence_artifact_envelope: 'wp-codebox/evidence-artifact-envelope/v1',
      artifact_result_envelope: 'wp-codebox/artifact-result-envelope/v1',
    },
  },
};

module.exports = {
  runtimeContractManifest() {
    return JSON.parse(JSON.stringify(manifest));
  },
  RUNTIME_CONTRACT_NORMALIZERS: {},
};
