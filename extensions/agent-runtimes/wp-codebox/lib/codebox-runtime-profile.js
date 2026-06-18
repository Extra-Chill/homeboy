'use strict';

const WP_CODEBOX_RUNTIME_PROFILE_SCHEMA = 'wp-codebox/runtime-profile/v1';
const WP_CODEBOX_PARENT_TOOL_BRIDGE_SCHEMA = 'wp-codebox/parent-tool-bridge/v1';

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
  return withoutEmptyObjectValues({
    ...normalizedProfile,
    ...normalizedRuntimeRequirements,
    schema: WP_CODEBOX_RUNTIME_PROFILE_SCHEMA,
    id: normalizedRuntimeRequirements.id || normalizedProfile.id || id,
    homeboy_profile_schema: normalizedProfile.schema && normalizedProfile.schema !== WP_CODEBOX_RUNTIME_PROFILE_SCHEMA ? normalizedProfile.schema : undefined,
    component_contracts: componentContracts,
    extra_plugins: componentContracts,
    runtime_overlays: runtimeOverlays,
    env: runtimeEnv,
    provider_plugins: providerPluginPaths.map((pluginPath) => ({ path: pluginPath })),
    runtime_state_mounts: runtimeStateMounts,
    runtime_config_mounts: runtimeConfigMounts,
    ...parentToolBridgeProfileFields(normalizedProfile, normalizedRuntimeRequirements, componentContracts),
  });
}

function parentToolBridgeProfileFields(profile = {}, runtimeRequirements = {}, componentContracts = []) {
  if (hasCodeboxParentToolBridge(profile, runtimeRequirements, componentContracts)) {
    return { homeboy_parent_tool_bridge: undefined };
  }
  return { homeboy_parent_tool_bridge: homeboyParentToolBridgeRequirement() };
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
    if (value.parent_tool_bridge && typeof value.parent_tool_bridge === 'object') {
      return true;
    }
    if (Array.isArray(value.parent_tool_bridges) && value.parent_tool_bridges.length > 0) {
      return true;
    }
    for (const key of ['components', 'runtime_components', 'component_contracts', 'extra_plugins']) {
      if (Array.isArray(value[key])) {
        queue.push(...value[key]);
      }
    }
  }
  return false;
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
  hasCodeboxParentToolBridge,
  homeboyParentToolBridgeRequirement,
};
