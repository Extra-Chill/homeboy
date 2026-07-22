'use strict';

const path = require('node:path');
const { existsSync } = require('node:fs');
const { fileURLToPath, pathToFileURL } = require('node:url');

const DEFAULT_CODEBOX_CONTRACTS_MODULE = '@automattic/wp-codebox-core/contracts';
const RUNTIME_CONTRACT_MANIFEST_SCHEMA = 'wp-codebox/runtime-contract-manifest/v1';

const REQUIRED_RUNTIME_CONTRACT_PATHS = [
  'schemas.providerRuntime.invocation',
  'schemas.providerRuntime.credentialRequirements',
  'schemas.providerRuntime.credentialPreflight',
  'schemas.providerRuntime.credentialResolution',
  'schemas.agentTask.runRequest',
  'schemas.agentTask.runResult',
  'schemas.runtimeBoundary.profile',
  'schemas.runtimeBoundary.previewLease',
  'schemas.runtimeBoundary.browserContainedSiteStatus',
  'schemas.runtimeBoundary.browserContainedSiteOpen',
  'schemas.runtimeBoundary.browserSessionProductDto',
  'schemas.runtimeBoundary.browserPreviewBootConfig',
  'schemas.artifact.resultEnvelope',
  'schemas.runnerWorkspace.prepareRequest',
  'schemas.runnerWorkspace.prepareResult',
  'schemas.runnerWorkspace.captureRequest',
  'schemas.runnerWorkspace.captureResult',
  'schemas.runnerWorkspace.commandRequest',
  'schemas.runnerWorkspace.commandResult',
  'schemas.runnerWorkspace.publicationRequest',
  'schemas.runnerWorkspace.publicationResult',
  'schemas.fanoutAggregation.input',
  'schemas.fanoutAggregation.output',
  'schemas.wordpressRuntime.fuzzSuite',
  'schemas.wordpressRuntime.fuzzSuiteResult',
  'schemas.wordpressRuntime.workloadRun',
  'abilities.wordpressRuntime.runFuzzSuite',
  'abilities.wordpressRuntime.runWorkload',
  'commands.wordpressRuntime.runFuzzSuite',
  'commands.wordpressRuntime.runWorkload',
  'capabilities.wordpressRuntime.commands.runFuzzSuite',
  'capabilities.wordpressRuntime.commands.runWorkload',
  'readiness.wordpressRuntime.schema',
  'readiness.wordpressRuntime.entrypoint',
  'readiness.wordpressRuntime.mode',
  'providerRuntime.schema',
  'providerRuntime.tasks.workspacePrepare',
  'providerRuntime.tasks.workspaceCapture',
  'providerRuntime.tasks.workspaceCommand',
  'providerRuntime.tasks.workspacePublish',
  'providerRuntime.tasks.toolCallTranscriptRecord',
  'providerRuntime.tasks.artifactHandoff',
  'providerRuntime.abilities.workspacePrepare',
  'providerRuntime.abilities.workspaceCapture',
  'providerRuntime.abilities.workspaceCommand',
  'providerRuntime.abilities.workspacePublish',
  'providerRuntime.abilities.toolCallTranscriptRecord',
  'providerRuntime.abilities.artifactHandoff',
  'providerRuntime.result_schemas.workspace_prepare',
  'providerRuntime.result_schemas.workspace_capture',
  'providerRuntime.result_schemas.workspace_command',
  'providerRuntime.result_schemas.workspace_publication',
  'providerRuntime.result_schemas.tool_call_transcript',
  'providerRuntime.result_schemas.evidence_artifact_envelope',
  'providerRuntime.result_schemas.artifact_result_envelope',
];

function runtimeContractManifest() {
  return clone(loadCanonicalRuntimeContractSourceSync({ required: true }).manifest);
}

function runtimeContractSchemas() {
  return runtimeContractManifest().schemas;
}

function providerRuntimeInvocationContract() {
  return runtimeContractManifest().providerRuntime;
}

async function loadRuntimeContractSource(options = {}) {
  return loadCanonicalRuntimeContractSource({ ...options, required: true });
}

async function loadCanonicalRuntimeContractSource(options = {}) {
  const errors = [];
  for (const specifier of coreModuleCandidates(options)) {
    try {
      const core = moduleExports(await import(specifier));
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

function loadCanonicalRuntimeContractSourceSync(options = {}) {
  const errors = [];
  for (const specifier of coreModuleCandidates(options)) {
    try {
      const core = moduleExports(require(requireSpecifier(specifier)));
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
    const error = new Error('WP Codebox canonical runtime contract manifest is unavailable. Install @automattic/wp-codebox-core or configure HOMEBOY_WP_CODEBOX_CORE_MODULE.');
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
  for (const pathName of REQUIRED_RUNTIME_CONTRACT_PATHS) {
    if (typeof valueAtPath(manifest, pathName) !== 'string') {
      throw new Error(`WP Codebox canonical runtime contract manifest missing ${pathName}.`);
    }
  }
}

function coreModuleCandidates(options = {}) {
  const explicit = options.wpCodeboxCoreModule || options.coreModule || process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE;
  const installDir = options.wpCodeboxInstallDir || process.env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(process.env.HOME || '', '.cache', 'homeboy', 'wp-codebox');
  const candidates = [
    explicit,
    path.join(installDir, 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js'),
    path.join(installDir, 'release', 'wp-codebox-cli', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js'),
    DEFAULT_CODEBOX_CONTRACTS_MODULE,
    'wp-codebox-workspace/contracts',
  ].filter(Boolean);
  return [...new Set(candidates.map(normalizeCoreModuleSpecifier))];
}

function normalizeCoreModuleSpecifier(specifier) {
  if (!specifier || specifier.startsWith('file:') || specifier.startsWith('node:')) {
    return specifier;
  }
  if (isPathSpecifier(specifier)) {
    return pathToFileURL(path.resolve(specifier)).href;
  }
  return specifier;
}

function isPathSpecifier(specifier) {
  return specifier.startsWith('.') || path.isAbsolute(specifier) || specifier.startsWith('~') || specifier.includes('\\') || existsSync(path.resolve(specifier));
}

function requireSpecifier(specifier) {
  return specifier?.startsWith('file:') ? fileURLToPath(specifier) : specifier;
}

function moduleExports(value) {
  if (!value || !value.default) {
    return value;
  }
  if (typeof value.default === 'object') {
    return { ...value.default, ...value };
  }
  return typeof value.runtimeContractManifest === 'function' ? value : value.default;
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function valueAtPath(value, pathName) {
  return pathName.split('.').reduce((current, part) => current?.[part], value);
}

module.exports = {
  REQUIRED_RUNTIME_CONTRACT_PATHS,
  RUNTIME_CONTRACT_MANIFEST_SCHEMA,
  coreModuleCandidates,
  loadCanonicalRuntimeContractSource,
  loadCanonicalRuntimeContractSourceSync,
  loadRuntimeContractSource,
  providerRuntimeInvocationContract,
  runtimeContractManifest,
  runtimeContractSchemas,
  validateCanonicalRuntimeContractManifest,
};
