'use strict';

const { normalizeRuntimeId, resolveRuntimeProvider } = require('./runtime-provider-resolver.cjs');

const RUNTIME_WORKFLOW_INPUTS_SCHEMA = 'homeboy/runtime-workflow-inputs/v1';

function renderRuntimeWorkflowInputs(options = {}) {
	const runtimeInput = options.runtimeId || options.runtime_id || options.runtimeProviderConfig?.id || options.runtime_provider_config?.id || options.runtime?.id || options.runtime || options.runtimeProvider || options.runtime_provider;
	const runtimeId = normalizeRuntimeId(runtimeInput);
	const runtime = options.runtimeProviderConfig || options.runtime_provider_config || resolveRuntimeProvider(runtimeId, options);
	const profileSelection = normalizeRuntimeProfileSelection(options.runtimeProfile || options.runtime_profile || options.profile);
	const runtimeProfiles = plainObject(options.runtimeProfiles || options.runtime_profiles);
	const selectedProfile = selectedRuntimeProfile(profileSelection, runtimeProfiles);
	const toolProfile = plainObject(options.toolProfile || options.tool_profile || options.sandboxToolPolicy || options.sandbox_tool_policy || options.toolPolicy || options.tool_policy);
	const adapter = runtimeWorkflowInputAdapter(runtime);
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
		workflow_inputs: stripUndefined(rendered.workflow_inputs || {}),
	});
}

function runtimeWorkflowInputAdapter(runtime) {
	if (isCodeboxRuntime(runtime)) {
		return renderCodeboxWorkflowInputs;
	}
	return renderDefaultWorkflowInputs;
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

function renderCodeboxWorkflowInputs({
	profileId,
	profile,
	runtimeProfiles,
	toolProfile,
	componentContracts,
	component_contracts,
	runtimeOverlays,
	runtime_overlays,
	runtimeEnv,
	runtime_env,
	providerPluginPaths,
	provider_plugin_paths,
	runtimeStateMounts,
	runtime_state_mounts,
	runtimeConfigMounts,
	runtime_config_mounts,
}) {
	const { codeboxRuntimeProfilePayload } = require('../../agent-runtimes/wp-codebox/lib/codebox-runtime-profile');
	const runtimeRequirements = codeboxRuntimeProfilePayload({
		id: profileId,
		profile,
		componentContracts: componentContracts || component_contracts || [],
		runtimeOverlays: runtimeOverlays || runtime_overlays || [],
		runtimeEnv: runtimeEnv || runtime_env || {},
		providerPluginPaths: providerPluginPaths || provider_plugin_paths || [],
		runtimeStateMounts: runtimeStateMounts || runtime_state_mounts,
		runtimeConfigMounts: runtimeConfigMounts || runtime_config_mounts,
	});

	return {
		runtime_requirements: runtimeRequirements,
		workflow_inputs: stripUndefined({
			runtime: 'wp-codebox',
			profile: profileId,
			runtime_profiles: {
				...runtimeProfiles,
				[profileId]: runtimeRequirements,
			},
			sandbox_tool_policy: Object.keys(toolProfile).length > 0 ? toolProfile : undefined,
		}),
	};
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

function selectedRuntimeProfile(selection, runtimeProfiles) {
	const profile = selection.profile || runtimeProfiles[selection.id] || {};
	if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
		throw new Error(`runtime profile ${selection.id} must be an object.`);
	}
	return { ...profile, id: profile.id || selection.id };
}

function isCodeboxRuntime(runtime) {
	return runtime?.id === 'wp-codebox' || runtime?.executor?.backend === 'codebox';
}

function plainObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	RUNTIME_WORKFLOW_INPUTS_SCHEMA,
	renderRuntimeWorkflowInputs,
	runtimeWorkflowInputAdapter,
};
