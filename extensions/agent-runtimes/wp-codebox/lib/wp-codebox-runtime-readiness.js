'use strict';

const fs = require('node:fs');
const path = require('node:path');

const {
  coreModuleCandidates,
  loadCanonicalRuntimeContractSourceSync,
} = require('./wp-codebox-runtime-contract-source');

const RUNTIME_READINESS_FAILURE_CLASS = 'codebox.preflight.runtime_readiness';
const RUNTIME_CONTRACT_FAILURE_CLASS = 'codebox.preflight.runtime_contract_unavailable';
const RUNTIME_OVERLAY_FAILURE_CLASS = 'codebox.preflight.runtime_overlay_dependency_unprepared';
const OWNER_SURFACE = 'wp-codebox-runtime-integration';

class WpCodeboxRuntimeReadinessError extends Error {
  constructor(diagnostics) {
    super(diagnostics[0]?.message || 'WP Codebox runtime readiness preflight failed.');
    this.name = 'WpCodeboxRuntimeReadinessError';
    this.diagnostics = diagnostics;
  }
}

function wpCodeboxRuntimeReadinessDiagnostics(taskInput = {}, options = {}) {
  return [
    ...runtimeContractReadinessDiagnostics(options),
    ...runtimeOverlayReadinessDiagnostics(taskInput, options),
  ];
}

function assertWpCodeboxRuntimeReady(taskInput = {}, options = {}) {
  const diagnostics = wpCodeboxRuntimeReadinessDiagnostics(taskInput, options);
  if (diagnostics.length > 0) {
    throw new WpCodeboxRuntimeReadinessError(diagnostics);
  }
}

function runtimeContractReadinessDiagnostics(options = {}) {
  try {
    loadCanonicalRuntimeContractSourceSync({ ...options, required: true });
    return [];
  } catch (error) {
    return [runtimeContractDiagnostic(error, options)];
  }
}

function runtimeContractDiagnostic(error, options = {}) {
  const explicitModule = options.wpCodeboxCoreModule || options.coreModule || process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE || '';
  const explicitPathMissing = explicitModule && isPathLike(explicitModule) && !fs.existsSync(resolvePath(explicitModule));
  const message = explicitPathMissing
    ? `WP Codebox canonical runtime contract manifest is unavailable. HOMEBOY_WP_CODEBOX_CORE_MODULE points to missing path: ${explicitModule}.`
    : 'WP Codebox canonical runtime contract manifest is unavailable. Install @automattic/wp-codebox-core or configure HOMEBOY_WP_CODEBOX_CORE_MODULE.';

  return {
    class: RUNTIME_CONTRACT_FAILURE_CLASS,
    message,
    data: {
      phase: 'codebox.preflight',
      owner_surface: OWNER_SURFACE,
      remediation: explicitPathMissing
        ? `Set HOMEBOY_WP_CODEBOX_CORE_MODULE to a valid WP Codebox contracts module path or install @automattic/wp-codebox-core. Missing path: ${explicitModule}`
        : 'Install @automattic/wp-codebox-core or set HOMEBOY_WP_CODEBOX_CORE_MODULE to the canonical WP Codebox contracts module path.',
      env: 'HOMEBOY_WP_CODEBOX_CORE_MODULE',
      configured_module: explicitModule,
      checked_candidates: coreModuleCandidates(options),
      errors: normalizeContractErrors(error?.wpCodeboxRuntimeContractErrors),
    },
  };
}

function runtimeOverlayReadinessDiagnostics(taskInput = {}, options = {}) {
  const overlays = normalizeArray(options.runtimeOverlays || taskInput.runtime_overlays || taskInput.runtimeOverlays);
  return overlays.flatMap((overlay, index) => runtimeOverlayDiagnostics(overlay, index));
}

function runtimeOverlayDiagnostics(overlay, index) {
  if (!overlay || typeof overlay !== 'object' || Array.isArray(overlay)) {
    return [];
  }

  const source = overlay.source || overlay.path || '';
  const library = overlay.library || overlay.name || overlay.slug || '';
  const requiresComposer = overlay.requires_composer === true
    || overlay.requiresComposer === true
    || library === 'php-ai-client'
    || fs.existsSync(path.join(resolvePath(source), 'composer.json'));
  if (!requiresComposer) {
    return [];
  }
  if (!source) {
    return [runtimeOverlayDiagnostic(index, overlay, 'Runtime overlay requires Composer dependencies but does not define a source path.', '')];
  }

  const resolvedSource = resolvePath(source);
  if (!fs.existsSync(resolvedSource)) {
    return [];
  }
  const composerJson = path.join(resolvedSource, 'composer.json');
  if (!fs.existsSync(composerJson)) {
    return [runtimeOverlayDiagnostic(index, overlay, `Runtime overlay source path is missing composer.json: ${source}.`, resolvedSource)];
  }
  if (!fs.existsSync(path.join(resolvedSource, 'vendor', 'autoload.php'))) {
    return [runtimeOverlayDiagnostic(index, overlay, `Runtime overlay Composer dependencies are not installed: ${source}/vendor/autoload.php is missing.`, resolvedSource)];
  }
  if (library === 'php-ai-client') {
    const providerMetadataPath = path.join(resolvedSource, 'src', 'Providers', 'DTO', 'ProviderMetadata.php');
    if (!fs.existsSync(providerMetadataPath)) {
      return [runtimeOverlayDiagnostic(index, overlay, `PHP AI Client runtime overlay is missing ProviderMetadata.php: ${providerMetadataPath}.`, resolvedSource)];
    }
    const providerMetadata = fs.readFileSync(providerMetadataPath, 'utf8');
    if (!/function\s+getDescription\s*\(/.test(providerMetadata)) {
      return [runtimeOverlayDiagnostic(index, overlay, `PHP AI Client runtime overlay does not expose ProviderMetadata::getDescription(): ${providerMetadataPath}.`, resolvedSource)];
    }
  }
  return [];
}

function runtimeOverlayDiagnostic(index, overlay, message, resolvedSource) {
  const source = overlay?.source || overlay?.path || '';
  const setupCommand = source ? `composer install --working-dir=${source}` : '';
  return {
    class: RUNTIME_OVERLAY_FAILURE_CLASS,
    message,
    data: {
      phase: 'codebox.preflight',
      owner_surface: OWNER_SURFACE,
      overlay_index: index,
      overlay,
      source,
      resolved_source: resolvedSource,
      expected: 'runtime overlay source with prepared Composer vendor/autoload.php',
      setup_command: setupCommand,
      remediation: setupCommand ? `Prepare the runtime overlay before dispatch: ${setupCommand}` : 'Provide a prepared runtime overlay source path before dispatch.',
    },
  };
}

function normalizeContractErrors(errors) {
  return normalizeArray(errors).map((entry) => ({
    specifier: entry?.specifier || '',
    message: entry?.message || entry?.error?.message || '',
  }));
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : (value === undefined || value === null || value === '' ? [] : [value]);
}

function resolvePath(value) {
  const raw = String(value || '');
  if (raw.startsWith('~/')) {
    return path.join(process.env.HOME || '', raw.slice(2));
  }
  return raw ? path.resolve(raw) : '';
}

function isPathLike(value) {
  const raw = String(value || '');
  return raw.startsWith('.') || raw.startsWith('/') || raw.startsWith('~/') || raw.includes('\\');
}

module.exports = {
  OWNER_SURFACE,
  RUNTIME_CONTRACT_FAILURE_CLASS,
  RUNTIME_OVERLAY_FAILURE_CLASS,
  RUNTIME_READINESS_FAILURE_CLASS,
  WpCodeboxRuntimeReadinessError,
  assertWpCodeboxRuntimeReady,
  runtimeContractReadinessDiagnostics,
  runtimeOverlayReadinessDiagnostics,
  wpCodeboxRuntimeReadinessDiagnostics,
};
