'use strict';

const fs = require('node:fs');
const path = require('node:path');

const RUNTIME_MANIFEST_PATH = path.resolve(__dirname, '..', 'wp-codebox.json');

function readJsonFile(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function runtimeExecutorManifest() {
  return readJsonFile(RUNTIME_MANIFEST_PATH)?.agent_task_executors?.[0] || {};
}

function providerPreflightManifests() {
  const manifests = runtimeExecutorManifest().provider_preflight;
  return manifests && typeof manifests === 'object' && !Array.isArray(manifests) ? manifests : {};
}

function providerPreflightManifest(provider) {
  const manifest = providerPreflightManifests()[provider];
  return manifest && typeof manifest === 'object' && !Array.isArray(manifest) ? manifest : null;
}

function normalizeStringArray(value) {
  if (Array.isArray(value)) {
    return value.filter((item) => typeof item === 'string' && item.trim()).map((item) => item.trim());
  }
  return typeof value === 'string' && value.trim() ? [value.trim()] : [];
}

function providerRequiredSecretEnv(provider) {
  return normalizeStringArray(providerPreflightManifest(provider)?.required_secret_env);
}

function providerOptionalSecretEnv(provider) {
  return normalizeStringArray(providerPreflightManifest(provider)?.optional_secret_env);
}

function providerSecretEnv(provider) {
  return Array.from(new Set([
    ...providerRequiredSecretEnv(provider),
    ...providerOptionalSecretEnv(provider),
  ]));
}

function providerAuthEnvSources(provider) {
  const sources = providerPreflightManifest(provider)?.auth_env_sources;
  return sources && typeof sources === 'object' && !Array.isArray(sources) ? sources : {};
}

function providerDiagnosticClass(provider) {
  return providerPreflightManifest(provider)?.diagnostic_class || '';
}

function providerLabel(provider) {
  return providerPreflightManifest(provider)?.label || provider;
}

function providerGuidance(provider) {
  return providerPreflightManifest(provider)?.guidance || '';
}

function providerPluginValidation(provider) {
  const validation = providerPreflightManifest(provider)?.provider_plugin_validation;
  return validation && typeof validation === 'object' && !Array.isArray(validation) ? validation : null;
}

function secretEnvNames(request) {
  return Array.from(new Set([
    ...normalizeStringArray(request?.secret_env),
    ...normalizeStringArray(request?.recipe?.secret_env),
  ]));
}

function missingRequiredSecretEnvMapping(request, provider) {
  const names = secretEnvNames(request);
  return providerRequiredSecretEnv(provider).filter((name) => !names.includes(name));
}

function missingRequiredSecretEnvValues(provider, env = process.env) {
  return providerRequiredSecretEnv(provider).filter((name) => !env[name]);
}

function providerAuthError(provider, message, missing = []) {
  const guidance = providerGuidance(provider);
  const suffix = guidance ? ` ${guidance}` : '';
  const error = new Error(`${providerLabel(provider)} provider auth preflight failed: ${message}${suffix}`);
  error.provider = provider;
  error.diagnosticClass = providerDiagnosticClass(provider);
  error.missingEnv = missing;
  return error;
}

function assertProviderSecretEnvPreflight(request, provider, env = process.env) {
  const missingMapping = missingRequiredSecretEnvMapping(request, provider);
  if (missingMapping.length > 0) {
    throw providerAuthError(provider, `missing required secret environment mapping: ${missingMapping.join(', ')}`, missingMapping);
  }

  const missing = missingRequiredSecretEnvValues(provider, env);
  if (missing.length > 0) {
    throw providerAuthError(provider, `missing required secret environment value: ${missing.join(', ')}`, missing);
  }
}

module.exports = {
  providerAuthEnvSources,
  providerDiagnosticClass,
  providerGuidance,
  providerLabel,
  providerOptionalSecretEnv,
  providerPluginValidation,
  providerPreflightManifest,
  providerPreflightManifests,
  providerRequiredSecretEnv,
  providerSecretEnv,
  assertProviderSecretEnvPreflight,
  missingRequiredSecretEnvMapping,
  missingRequiredSecretEnvValues,
  normalizeStringArray,
};
