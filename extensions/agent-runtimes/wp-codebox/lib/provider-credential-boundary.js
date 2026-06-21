'use strict';

const WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA = 'wp-codebox/provider-credential-boundary/v1';

function providerCredentialBoundary() {
  return {
    schema: WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA,
    version: 1,
    transport: 'secret-env-names-only',
    resolution: 'provider-plugin-or-parent-control-plane',
    redaction: 'raw-provider-credentials-never-in-json-artifacts-or-diagnostics',
  };
}

function providerCredentialSecretEnvNames(...sources) {
  return Array.from(new Set(sources.flatMap(secretEnvNamesFromSource)));
}

function providerCredentialRequestFields(...sources) {
  const secretEnv = providerCredentialSecretEnvNames(...sources);
  return { secret_env: secretEnv, provider_credential_boundary: providerCredentialBoundary() };
}

function assertProviderCredentialBoundaryNamesOnly(request = {}) {
  const forbidden = ['secret_env_values', 'secretEnvValues', 'secret_values', 'secretValues', 'credentials'];
  const present = forbidden.filter((field) => request[field] !== undefined);
  if (present.length > 0) {
    throw new Error(`WP Codebox provider credential boundary accepts secret_env names only; remove raw credential fields: ${present.join(', ')}`);
  }
}

function secretEnvNamesFromSource(source) {
  if (!source || typeof source !== 'object' || Array.isArray(source)) {
    return [];
  }
  return [
    ...normalizeStringArray(source.secret_env),
    ...normalizeStringArray(source.secretEnv),
    ...normalizeStringArray(source.recipe?.secret_env),
    ...normalizeStringArray(source.recipe?.secretEnv),
  ];
}

function normalizeStringArray(value) {
  if (Array.isArray(value)) {
    return value.filter((item) => typeof item === 'string' && item.trim()).map((item) => item.trim());
  }
  return typeof value === 'string' && value.trim() ? [value.trim()] : [];
}

module.exports = {
  WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA,
  assertProviderCredentialBoundaryNamesOnly,
  providerCredentialBoundary,
  providerCredentialRequestFields,
  providerCredentialSecretEnvNames,
};
