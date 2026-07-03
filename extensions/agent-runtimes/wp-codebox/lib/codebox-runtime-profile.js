'use strict';

const {
  runtimeContractSchemas,
} = require('./wp-codebox-runtime-contract-source');

const RUNTIME_CONTRACT_SCHEMAS = runtimeContractSchemas();

const WP_CODEBOX_RUNTIME_PROFILE_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.profile;
const WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA = 'wp-codebox/parent-tool-bridge/v1';
const WP_CODEBOX_UPSTREAM_REQUIREMENT_SCHEMA = 'wp-codebox/upstream-primitive-requirement/v1';

function codeboxRuntimeProfilePayload({
  id,
  profile = {},
  runtimeRequirements = {},
  componentContracts = [],
  runtimeOverlays = [],
  runtimeEnv = {},
  runtimeMounts,
  providerPluginPaths = [],
  runtimeStateMounts,
  runtimeConfigMounts,
  normalizeRuntimeProfile,
  normalizeRuntimeProfilePayload,
} = {}) {
  const normalizedProfile = plainObject(profile);
  const normalizedRuntimeRequirements = plainObject(runtimeRequirements);
  const normalizedRuntimeOverlays = uniqueObjectsByRuntimeIdentity([
    ...normalizeArray(normalizedProfile.runtime_overlays),
    ...normalizeArray(normalizedRuntimeRequirements.runtime_overlays),
    ...normalizeArray(runtimeOverlays),
  ]);
  const normalizedRuntimeEnv = {
    ...plainObject(normalizedProfile.env),
    ...plainObject(normalizedProfile.runtime_env),
    ...plainObject(normalizedRuntimeRequirements.env),
    ...plainObject(normalizedRuntimeRequirements.runtime_env),
    ...plainObject(runtimeEnv),
  };
  const normalizedComponentContracts = codeboxRuntimeComponentContracts({
    profile: normalizedProfile,
    runtimeRequirements: normalizedRuntimeRequirements,
    componentContracts,
  });
  const normalizedProviderPlugins = uniqueObjectsByRuntimeIdentity([
    ...providerPluginEntries(normalizedProfile.provider_plugins),
    ...providerPluginEntries(normalizedRuntimeRequirements.provider_plugins),
    ...providerPluginPaths.map((pluginPath) => ({ path: pluginPath })),
  ]);
  const payload = withoutEmptyObjectValues({
    ...normalizedProfile,
    ...normalizedRuntimeRequirements,
    schema: WP_CODEBOX_RUNTIME_PROFILE_SCHEMA,
    id: normalizedRuntimeRequirements.id || normalizedProfile.id || id,
    homeboy_profile_schema: normalizedProfile.schema && normalizedProfile.schema !== WP_CODEBOX_RUNTIME_PROFILE_SCHEMA ? normalizedProfile.schema : undefined,
    component_contracts: normalizedComponentContracts,
    runtime_overlays: normalizedRuntimeOverlays,
    runtime_mounts: runtimeMounts,
    env: normalizedRuntimeEnv,
    provider_plugins: normalizedProviderPlugins,
    runtime_state_mounts: runtimeStateMounts,
    runtime_config_mounts: runtimeConfigMounts,
  });
  const coreNormalizer = firstFunction(normalizeRuntimeProfilePayload, normalizeRuntimeProfile);
  if (coreNormalizer) {
    const normalized = normalizeWithCoreRuntimeProfileNormalizer(coreNormalizer, payload, {
      id,
      profile: normalizedProfile,
      runtimeRequirements: normalizedRuntimeRequirements,
      componentContracts,
      runtimeOverlays,
      runtimeEnv,
      runtimeMounts,
      providerPluginPaths,
      runtimeStateMounts,
      runtimeConfigMounts,
    });
    if (isPlainObject(normalized)) {
      return normalized;
    }
  }
  return payload;
}

function providerPluginEntries(value) {
  return normalizeArray(value).flatMap((entry) => {
    if (typeof entry === 'string') {
      return [{ path: entry }];
    }
    return entry;
  });
}

function codeboxRuntimeComponentContracts({ profile = {}, runtimeRequirements = {}, componentContracts = [] } = {}) {
  return uniqueObjectsByRuntimeIdentity([
    ...normalizeArray(profile.component_contracts),
    ...normalizeArray(runtimeRequirements.component_contracts),
    ...normalizeArray(componentContracts),
  ]);
}

function hasCodeboxParentToolBridge(...values) {
  const queue = values.flatMap((value) => Array.isArray(value) ? value : [value]);
  while (queue.length > 0) {
    const value = queue.shift();
    if (!value || typeof value !== 'object') {
      continue;
    }
    if (value.schema === WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA) {
      return true;
    }
    if (parentToolBridgeDescriptor(value)) {
      return true;
    }
    if (value.parent_tool_bridge && typeof value.parent_tool_bridge === 'object') {
      return true;
    }
    if (Array.isArray(value.parent_tool_bridges) && value.parent_tool_bridges.length > 0) {
      return true;
    }
    for (const key of ['components', 'plugins', 'mu_plugins', 'themes', 'overlays', 'runtime_components', 'component_contracts', 'extra_plugins']) {
      if (Array.isArray(value[key])) {
        queue.push(...value[key]);
      }
    }
  }
  return false;
}

function parentToolBridgeDescriptor(value) {
  const identifiers = [
    value.kind,
    value.type,
    value.id,
    value.slug,
    value.capability,
    value.metadata?.kind,
    value.metadata?.type,
  ].filter(Boolean).map((entry) => String(entry).toLowerCase());
  return identifiers.some((entry) => entry === 'parent-tool-bridge' || entry === 'parent_tool_bridge' || entry === 'parent_tool_bridge_component')
    || value.metadata?.parent_tool_bridge === true
    || value.metadata?.parentToolBridge === true
    || normalizeArray(value.capabilities).some((capability) => String(capability).toLowerCase() === 'parent_tool_bridge' || String(capability).toLowerCase() === 'parent-tool-bridge');
}

function codeboxParentToolBridgeRequirement() {
  return {
    schema: WP_CODEBOX_UPSTREAM_REQUIREMENT_SCHEMA,
    id: 'parent-tool-bridge',
    owner: 'wp-codebox',
    primitive_schema: WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA,
    status: 'required-upstream-primitive',
    adapter_behavior: 'declare_requirement_only',
    requirement: 'Expose parent-owned tools inside the sandbox through a Codebox-owned bridge component declared by the public parent-tool-bridge contract.',
  };
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function normalizeWithCoreRuntimeProfileNormalizer(normalizer, payload, context = {}) {
  try {
    return normalizer(payload, context);
  } catch {
    try {
      return normalizer(context);
    } catch {
      return null;
    }
  }
}

function firstFunction(...values) {
  return values.find((value) => typeof value === 'function') || null;
}

function uniqueObjectsByRuntimeIdentity(entries) {
  const seen = new Map();
  const merged = [];
  for (const entry of normalizeArray(entries)) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      continue;
    }
    const key = [entry.slug, entry.id, entry.path || entry.source || entry.target, entry.kind, entry.type].filter(Boolean).join(':');
    if (!key) {
      merged.push(entry);
      continue;
    }
    const existingIndex = seen.get(key);
    if (existingIndex !== undefined) {
      merged[existingIndex] = withoutEmptyObjectValues({
        ...merged[existingIndex],
        ...entry,
        metadata: {
          ...plainObject(merged[existingIndex].metadata),
          ...plainObject(entry.metadata),
        },
      });
      continue;
    }
    seen.set(key, merged.length);
    merged.push(entry);
  }
  return merged;
}

function withoutEmptyObjectValues(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => {
    if (Array.isArray(entry)) {
      return entry.length > 0;
    }
    if (entry && typeof entry === 'object') {
      return Object.keys(entry).length > 0;
    }
    return entry !== undefined && entry !== '';
  }));
}

module.exports = {
  WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA,
  WP_CODEBOX_RUNTIME_PROFILE_SCHEMA,
  WP_CODEBOX_UPSTREAM_REQUIREMENT_SCHEMA,
  codeboxRuntimeComponentContracts,
  codeboxRuntimeProfilePayload,
  codeboxParentToolBridgeRequirement,
  hasCodeboxParentToolBridge,
};
