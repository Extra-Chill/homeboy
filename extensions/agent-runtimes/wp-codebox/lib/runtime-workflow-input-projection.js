'use strict';

const { codeboxRuntimeProfilePayload } = require('./codebox-runtime-profile');

function renderRuntimeWorkflowInputs({
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

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = { renderRuntimeWorkflowInputs };
