'use strict';

const WP_CODEBOX_RUNTIME_PROFILE_SCHEMA = 'wp-codebox/runtime-profile/v1';
const WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA = 'wp-codebox/parent-tool-bridge/v1';

const RUNTIME_PROFILE_DEPENDENCY_FIELDS = ['components', 'plugins', 'mu_plugins', 'themes', 'overlays'];

const HOMEBOY_PARENT_TOOL_BRIDGE_ENV = [
  'HOMEBOY_AGENT_TOOL_POLICY_JSON',
  'HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA',
  'HOMEBOY_AGENT_TOOL_RESULT_SCHEMA',
  'HOMEBOY_AGENT_TOOL_POLICY_SCHEMA',
];

function codeboxRuntimeProfilePayload({
  id,
  profile = {},
  runtimeRequirements = {},
  componentContracts = [],
  runtimeOverlays = [],
  runtimeEnv = {},
  providerPluginPaths = [],
  runtimeStateMounts,
  runtimeConfigMounts,
} = {}) {
  const normalizedProfile = plainObject(profile);
  const normalizedRuntimeRequirements = plainObject(runtimeRequirements);
  const runtimeProfileDependencies = runtimeProfileDependencyFields(normalizedProfile, normalizedRuntimeRequirements);
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
  const normalizedComponentContracts = uniqueObjectsByRuntimeIdentity([
    ...normalizeArray(normalizedProfile.component_contracts),
    ...normalizeArray(normalizedRuntimeRequirements.component_contracts),
    ...componentContractsFromRuntimeProfileDependencies(runtimeProfileDependencies),
    ...normalizeArray(componentContracts),
  ]);
  const normalizedProviderPlugins = uniqueObjectsByRuntimeIdentity([
    ...providerPluginEntries(normalizedProfile.provider_plugins),
    ...providerPluginEntries(normalizedRuntimeRequirements.provider_plugins),
    ...providerPluginPaths.map((pluginPath) => ({ path: pluginPath })),
  ]);
  return withoutEmptyObjectValues({
    ...normalizedProfile,
    ...normalizedRuntimeRequirements,
    schema: WP_CODEBOX_RUNTIME_PROFILE_SCHEMA,
    id: normalizedRuntimeRequirements.id || normalizedProfile.id || id,
    homeboy_profile_schema: normalizedProfile.schema && normalizedProfile.schema !== WP_CODEBOX_RUNTIME_PROFILE_SCHEMA ? normalizedProfile.schema : undefined,
    ...runtimeProfileDependencies,
    component_contracts: normalizedComponentContracts,
    extra_plugins: normalizedComponentContracts,
    runtime_overlays: normalizedRuntimeOverlays,
    env: normalizedRuntimeEnv,
    provider_plugins: normalizedProviderPlugins,
    runtime_state_mounts: runtimeStateMounts,
    runtime_config_mounts: runtimeConfigMounts,
    ...parentToolBridgeProfileFields(normalizedProfile, normalizedRuntimeRequirements, runtimeProfileDependencies, normalizedComponentContracts),
  });
}

function providerPluginEntries(value) {
  return normalizeArray(value).flatMap((entry) => {
    if (typeof entry === 'string') {
      return [{ path: entry }];
    }
    return entry;
  });
}

function parentToolBridgeProfileFields(profile = {}, runtimeRequirements = {}, runtimeProfileDependencies = {}, componentContracts = []) {
  if (hasCodeboxParentToolBridge(profile, runtimeRequirements, runtimeProfileDependencies, componentContracts)) {
    return { homeboy_parent_tool_bridge: undefined };
  }
  return { homeboy_parent_tool_bridge: homeboyParentToolBridgeRequirement() };
}

function runtimeProfileDependencyFields(profile = {}, runtimeRequirements = {}) {
  return Object.fromEntries(RUNTIME_PROFILE_DEPENDENCY_FIELDS.map((field) => [
    field,
    uniqueObjectsByRuntimeIdentity([
      ...normalizeArray(profile[field]),
      ...normalizeArray(runtimeRequirements[field]),
    ]),
  ]));
}

function componentContractsFromRuntimeProfileDependencies(dependencies = {}) {
  return [
    ...normalizeArray(dependencies.components).map((dependency) => componentContractFromDependency(dependency, 'mu-plugin')),
    ...normalizeArray(dependencies.mu_plugins).map((dependency) => componentContractFromDependency(dependency, 'mu-plugin')),
    ...normalizeArray(dependencies.plugins).map((dependency) => componentContractFromDependency(dependency, 'plugin')),
  ].filter(Boolean);
}

function componentContractFromDependency(dependency, loadAs) {
  if (!dependency || typeof dependency !== 'object' || Array.isArray(dependency)) {
    return null;
  }
  const source = dependency.source || dependency.path;
  if (!dependency.slug || !source) {
    return null;
  }
  return withoutEmptyObjectValues({
    slug: dependency.slug,
    path: dependency.path || source,
    source,
    pluginFile: dependency.pluginFile || dependency.plugin_file,
    loadAs: dependency.loadAs || dependency.load_as || loadAs,
    activate: dependency.activate,
    required: dependency.required,
    readiness_probe: dependency.readiness_probe || dependency.readinessProbe,
    metadata: dependency.metadata,
  });
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
    for (const key of [...RUNTIME_PROFILE_DEPENDENCY_FIELDS, 'runtime_components', 'component_contracts', 'extra_plugins']) {
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

function homeboyParentToolBridgeRequirement() {
  return {
    schema: WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA,
    status: 'declared-compatibility-bridge',
    env: HOMEBOY_PARENT_TOOL_BRIDGE_ENV,
    upstream_expected: 'Codebox runtime profiles should expose a parent-tool bridge component that maps parent-owned tools into sandbox-visible tool descriptors without Homeboy injecting bridge env directly.',
  };
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function uniqueObjectsByRuntimeIdentity(entries) {
  const seen = new Set();
  return normalizeArray(entries).filter((entry) => {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      return false;
    }
    const key = [entry.slug, entry.id, entry.path || entry.source || entry.target, entry.kind, entry.type].filter(Boolean).join(':');
    if (!key) {
      return true;
    }
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
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
  codeboxRuntimeProfilePayload,
  componentContractsFromRuntimeProfileDependencies,
  hasCodeboxParentToolBridge,
  homeboyParentToolBridgeRequirement,
};
