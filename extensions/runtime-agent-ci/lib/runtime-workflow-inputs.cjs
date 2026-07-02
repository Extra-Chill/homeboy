'use strict';

const path = require('node:path');

const { normalizeRuntimeId, resolveRuntimeProvider } = require('./runtime-provider-resolver.cjs');
const {
  expandAgentTaskCapabilityBundles,
  expandAgentTaskToolPresets,
} = require('../../agent-task-contracts');

const RUNTIME_WORKFLOW_INPUTS_SCHEMA = 'homeboy/runtime-workflow-inputs/v1';

function renderRuntimeWorkflowInputs(options = {}) {
	const runtimeInput = options.runtime_id || options.runtime?.id || options.runtime;
	const runtimeId = normalizeRuntimeId(runtimeInput);
	const runtime = runtimeFromOptions(runtimeId, options);
	const workloadProfile = namedProfile(options.workloadProfile || options.workload_profile, workloadProfiles(runtime));
	const profileSelection = normalizeRuntimeProfileSelection(options.runtime_profile || workloadProfile.runtime_profile);
	const runtimeProfiles = plainObject(options.runtime_profiles);
	const selectedProfile = selectedRuntimeProfile(profileSelection, runtimeProfiles, runtimeRequirementFields({ ...workloadProfile, ...options }));
	const toolProfile = resolveToolProfile({ ...workloadProfile, ...options, toolProfileName: options.toolProfileName || options.tool_profile_name || workloadProfile.tool_profile }, runtime, workloadProfile);
	const adapter = runtimeWorkflowInputAdapter(runtime, options);
	const rendered = adapter({
		...options,
		runtime,
		runtimeId,
		profileId: profileSelection.id,
		profile: selectedProfile,
		runtimeProfiles,
		toolProfile,
	});

	return stripUndefined({
		schema: RUNTIME_WORKFLOW_INPUTS_SCHEMA,
		runtime_id: runtime.id || runtimeId,
		runtime_profile: profileSelection.id,
		runtime_profiles: {
			...runtimeProfiles,
			[profileSelection.id]: rendered.runtime_requirements || selectedProfile,
		},
		runtime_requirements: rendered.runtime_requirements || selectedProfile,
		tool_profile: Object.keys(toolProfile).length > 0 ? toolProfile : undefined,
		workload_profile: workloadProfile.id,
		workflow_inputs: stripUndefined(rendered.workflow_inputs || {}),
	});
}

function runtimeFromOptions(runtimeId, options = {}) {
	if (isPlainObject(options.runtime)) {
		return options.runtime;
	}
	if (isPlainObject(options.runtimeProviderConfig)) {
		return options.runtimeProviderConfig;
	}
	if (isPlainObject(options.runtime_provider_config)) {
		return options.runtime_provider_config;
	}
	return resolveRuntimeProvider(runtimeId, options);
}

function runtimeWorkflowInputAdapter(runtime, options = {}) {
	const adapter = runtime?.manifest?.workflow_input_projection?.adapter;
	if (!isPlainObject(adapter) || !adapter.module) {
		return renderDefaultWorkflowInputs;
	}
	const repoRoot = options.repoRoot || path.resolve(__dirname, '..', '..');
	const adapterPath = path.resolve(repoRoot, adapter.module);
	const loaded = require(adapterPath);
	const exportName = adapter.export || 'renderRuntimeWorkflowInputs';
	if (typeof loaded[exportName] !== 'function') {
		throw new Error(`Runtime ${runtime.id} workflow input adapter ${adapter.module} does not export ${exportName}`);
	}
	return loaded[exportName];
}

function renderDefaultWorkflowInputs({ runtime, profileId, profile, runtimeProfiles }) {
	const effectiveRuntimeProfiles = {
		...runtimeProfiles,
		[profileId]: profile,
	};
	return {
		runtime_requirements: profile,
		workflow_inputs: {
			runtime: runtime.id,
			profile: profileId,
			runtime_profiles: effectiveRuntimeProfiles,
		},
	};
}

function resolveToolProfile(options = {}, runtime = {}, workloadProfile = {}) {
	const profile = namedProfile(options.toolProfileName || options.tool_profile_name || options.toolProfile || options.tool_profile, toolProfiles(runtime));
	const inline = plainObject(options.toolProfile || options.tool_profile || options.sandboxToolPolicy || options.sandbox_tool_policy || options.toolPolicy || options.tool_policy);
	const capabilityExpansion = expandAgentTaskCapabilityBundles(profile.capability_bundles || workloadProfile.capability_bundles || []);
	const toolPresetTools = expandAgentTaskToolPresets([...(capabilityExpansion.tool_presets || []), ...normalizeArray(profile.tool_presets), ...normalizeArray(workloadProfile.tool_presets)]);
	return stripUndefined({
		...profile,
		...inline,
		workspace_tools: mergeProfileObjects(toolPresetTools.workspace_tools, profile.workspace_tools, inline.workspace_tools),
		publication_tools: normalizeArray(inline.publication_tools).length > 0 ? inline.publication_tools : (profile.publication_tools || toolPresetTools.publication_tools),
		provider_runtime_invocation: mergeProfileObjects(capabilityExpansion.provider_runtime_invocation, profile.provider_runtime_invocation, inline.provider_runtime_invocation),
	});
}

function toolProfiles(runtime = {}) {
	return plainObject(runtime.manifest?.tool_profiles || runtime.manifest?.profiles?.tools);
}

function workloadProfiles(runtime = {}) {
	return plainObject(runtime.manifest?.workload_profiles || runtime.manifest?.profiles?.workloads);
}

function namedProfile(selection, profiles = {}) {
	if (typeof selection === 'string' && selection.trim() !== '') {
		const id = selection.trim();
		return { ...plainObject(profiles[id]), id };
	}
	if (selection && typeof selection === 'object' && !Array.isArray(selection)) {
		const id = selection.id || selection.name || selection.slug;
		return stripUndefined({ ...selection, id });
	}
	return {};
}

function mergeProfileObjects(...objects) {
	const merged = {};
	for (const object of objects) {
		if (object && typeof object === 'object' && !Array.isArray(object)) {
			Object.assign(merged, object);
		}
	}
	return Object.keys(merged).length > 0 ? merged : undefined;
}

function normalizeArray(value) {
	return Array.isArray(value) ? value : [];
}

function normalizeRuntimeProfileSelection(value) {
	if (typeof value === 'string' && value.trim() !== '') {
		return { id: value, profile: null };
	}
	if (value && typeof value === 'object' && !Array.isArray(value)) {
		const id = value.id || value.name || value.slug;
		if (typeof id !== 'string' || id.trim() === '') {
			throw new Error('runtime_profile object requires an id.');
		}
		return { id, profile: value };
	}
	throw new Error('runtime_profile is required.');
}

function selectedRuntimeProfile(selection, runtimeProfiles, requirementFields = {}) {
	const profile = selection.profile || runtimeProfiles[selection.id] || {};
	if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
		throw new Error(`runtime profile ${selection.id} must be an object.`);
	}
	return { ...profile, ...requirementFields, id: profile.id || selection.id };
}

function runtimeRequirementFields(options = {}) {
	return stripUndefined({
		runtime_mounts: options.runtimeMounts || options.runtime_mounts || options.mounts,
		runtime_state_mounts: options.runtimeStateMounts || options.runtime_state_mounts,
		runtime_config_mounts: options.runtimeConfigMounts || options.runtime_config_mounts,
	});
}

function plainObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function isPlainObject(value) {
	return Boolean(value && typeof value === 'object' && !Array.isArray(value));
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	RUNTIME_WORKFLOW_INPUTS_SCHEMA,
	renderRuntimeWorkflowInputs,
	runtimeWorkflowInputAdapter,
	resolveToolProfile,
};
