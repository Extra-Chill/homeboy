'use strict';

const path = require('node:path');
const { existsSync, readdirSync } = require('node:fs');
const { homedir } = require('node:os');
const { pathToFileURL } = require('node:url');

const DEFAULT_CODEBOX_CORE_MODULE = '@automattic/wp-codebox-core';
const RUNTIME_CORE_ENTRY = 'packages/runtime-core/dist/index.js';
const RUNTIME_CONTRACT_MANIFEST_SCHEMA = 'wp-codebox/runtime-contract-manifest/v1';

const FALLBACK_RUNTIME_CONTRACT_SCHEMAS = {
  providerRuntime: {
    invocation: 'wp-codebox/provider-runtime-invocation-contract/v1',
    credentialRequirements: 'wp-codebox/provider-credential-requirements/v1',
    credentialPreflight: 'wp-codebox/provider-credential-preflight/v1',
    credentialResolution: 'wp-codebox/provider-credential-resolution/v1',
  },
  agentTask: {
    runResult: 'wp-codebox/agent-task-run-result/v1',
  },
  runtimeBoundary: {
    profile: 'wp-codebox/runtime-profile/v1',
    previewLease: 'wp-codebox/preview-lease/v1',
    browserContainedSiteStatus: 'wp-codebox/browser-contained-site-status/v1',
    browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1',
    browserSessionProductDto: 'wp-codebox/browser-session-product/v1',
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
};

const FALLBACK_PROVIDER_RUNTIME_INVOCATION_CONTRACT = {
  schema: FALLBACK_RUNTIME_CONTRACT_SCHEMAS.providerRuntime.invocation,
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
    workspace_prepare: FALLBACK_RUNTIME_CONTRACT_SCHEMAS.runnerWorkspace.prepareResult,
    workspace_capture: FALLBACK_RUNTIME_CONTRACT_SCHEMAS.runnerWorkspace.captureResult,
    workspace_command: FALLBACK_RUNTIME_CONTRACT_SCHEMAS.runnerWorkspace.commandResult,
    workspace_publication: FALLBACK_RUNTIME_CONTRACT_SCHEMAS.runnerWorkspace.publicationResult,
    tool_call_transcript: 'wp-codebox/tool-call-transcript/v1',
    evidence_artifact_envelope: 'wp-codebox/evidence-artifact-envelope/v1',
  },
};

const FALLBACK_RUNTIME_CONTRACT_MANIFEST = {
  schema: RUNTIME_CONTRACT_MANIFEST_SCHEMA,
  version: 1,
  schemas: FALLBACK_RUNTIME_CONTRACT_SCHEMAS,
  providerRuntime: FALLBACK_PROVIDER_RUNTIME_INVOCATION_CONTRACT,
};

function runtimeContractManifest() {
  return clone(FALLBACK_RUNTIME_CONTRACT_MANIFEST);
}

function runtimeContractSchemas() {
  return runtimeContractManifest().schemas;
}

function providerRuntimeInvocationContract() {
  return runtimeContractManifest().providerRuntime;
}

async function loadRuntimeContractSource(options = {}) {
  const canonical = await loadCanonicalRuntimeContractSource(options);
  if (canonical) {
    return canonical;
  }
  return {
    source: 'homeboy-extensions-fallback',
    manifest: runtimeContractManifest(),
    normalizers: {},
    canonical: false,
  };
}

async function loadCanonicalRuntimeContractSource(options = {}) {
  const errors = [];
  for (const specifier of coreModuleCandidates(options)) {
    try {
      const core = await import(specifier);
      if (typeof core.runtimeContractManifest !== 'function') {
        errors.push({ specifier, message: 'missing runtimeContractManifest export' });
        continue;
      }
      const manifest = core.runtimeContractManifest();
      validateCanonicalRuntimeContractManifest(manifest);
      return {
        source: specifier,
        manifest,
        normalizers: core.RUNTIME_CONTRACT_NORMALIZERS || {},
        canonical: true,
      };
    } catch (error) {
      errors.push({ specifier, error, message: error.message });
    }
  }

  if (options.required) {
    const error = new Error('WP Codebox canonical runtime contract manifest is unavailable.');
    error.wpCodeboxRuntimeContractErrors = errors;
    throw error;
  }
  return null;
}

function validateCanonicalRuntimeContractManifest(manifest) {
  if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)) {
    throw new Error('WP Codebox canonical runtime contract manifest must be an object.');
  }
  if (manifest.schema !== RUNTIME_CONTRACT_MANIFEST_SCHEMA) {
    throw new Error(`WP Codebox canonical runtime contract manifest schema mismatch: ${manifest.schema || 'missing'}.`);
  }
  assertContractSubset(manifest.schemas, FALLBACK_RUNTIME_CONTRACT_SCHEMAS, 'schemas');
  assertContractSubset(manifest.providerRuntime, FALLBACK_PROVIDER_RUNTIME_INVOCATION_CONTRACT, 'providerRuntime');
}

function assertContractSubset(actual, expected, pathName) {
  if (!actual || typeof actual !== 'object' || Array.isArray(actual)) {
    throw new Error(`WP Codebox canonical runtime contract manifest missing ${pathName}.`);
  }
  for (const [key, expectedValue] of Object.entries(expected)) {
    const currentPath = `${pathName}.${key}`;
    const actualValue = actual[key];
    if (expectedValue && typeof expectedValue === 'object' && !Array.isArray(expectedValue)) {
      assertContractSubset(actualValue, expectedValue, currentPath);
    } else if (actualValue !== expectedValue) {
      throw new Error(`WP Codebox canonical runtime contract mismatch at ${currentPath}: expected ${expectedValue}, received ${actualValue || 'missing'}.`);
    }
  }
}

function coreModuleCandidates(options = {}) {
  const explicit = options.wpCodeboxCoreModule || options.coreModule || process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE;
  if (explicit) {
    return [normalizeCoreModuleSpecifier(explicit)];
  }

  const candidates = [DEFAULT_CODEBOX_CORE_MODULE, 'wp-codebox-workspace/core'];
  for (const candidate of setupCacheCoreModuleCandidates(options)) {
    if (existsSync(candidate) && !candidates.includes(candidate)) {
      candidates.push(candidate);
    }
  }
  for (const root of workspaceRoots(options)) {
    for (const repoPath of codeboxRepoCandidates(root)) {
      const runtimeCore = path.resolve(repoPath, RUNTIME_CORE_ENTRY);
      if (existsSync(runtimeCore) && !candidates.includes(runtimeCore)) {
        candidates.push(runtimeCore);
      }
    }
  }

  return candidates.map(normalizeCoreModuleSpecifier);
}

function setupCacheCoreModuleCandidates(options = {}) {
  const installRoot = options.wpCodeboxInstallDir || process.env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.resolve(homedir(), '.cache/homeboy/wp-codebox');
  return [
    path.resolve(installRoot, 'source', RUNTIME_CORE_ENTRY),
    path.resolve(installRoot, 'release/wp-codebox-cli', RUNTIME_CORE_ENTRY),
    path.resolve(installRoot, 'source/node_modules/@automattic/wp-codebox-core/dist/index.js'),
    path.resolve(installRoot, 'release/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist/index.js'),
  ];
}

function workspaceRoots(options = {}) {
	const repoRoot = path.resolve(__dirname, '..', '..', '..', '..');
  return [...new Set([
    options.workspaceRoot,
    process.env.HOMEBOY_WORKSPACE_ROOT,
    process.env.HOMEBOY_DEVELOPER_WORKSPACE,
    path.dirname(repoRoot),
  ].filter(Boolean))];
}

function codeboxRepoCandidates(root) {
  const exact = path.resolve(root, 'wp-codebox');
  const candidates = existsSync(exact) ? [exact] : [];
  try {
    candidates.push(...readdirSync(root, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && entry.name.startsWith('wp-codebox@'))
      .map((entry) => path.resolve(root, entry.name))
      .sort());
  } catch {
    // A missing or unreadable workspace root simply contributes no candidates.
  }
  return candidates;
}

function normalizeCoreModuleSpecifier(specifier) {
  if (!specifier || specifier.startsWith('file:') || specifier.startsWith('node:')) {
    return specifier;
  }
  if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.includes(path.sep)) {
    return pathToFileURL(path.resolve(specifier)).href;
  }
  return specifier;
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

module.exports = {
  FALLBACK_RUNTIME_CONTRACT_MANIFEST,
  FALLBACK_RUNTIME_CONTRACT_SCHEMAS,
  RUNTIME_CONTRACT_MANIFEST_SCHEMA,
  coreModuleCandidates,
  loadCanonicalRuntimeContractSource,
  loadRuntimeContractSource,
  providerRuntimeInvocationContract,
  runtimeContractManifest,
  runtimeContractSchemas,
  validateCanonicalRuntimeContractManifest,
};
