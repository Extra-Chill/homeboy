'use strict';

const WORDPRESS_FUZZ_COMMAND_MANIFEST_SCHEMA = 'homeboy/wordpress-fuzz-command-manifest/v1';

const DISPOSABLE_DESTRUCTIVE_CAPABILITIES = Object.freeze([
	'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission',
	'mutation-isolation-artifact', 'delete-boundary-artifact', 'sandbox-isolation-proof', 'artifact-export',
]);

// Intent policy belongs to WordPress; identifiers come from the runtime descriptor.
const CASE_INTENT_REQUIREMENTS = Object.freeze({
	default: Object.freeze({ operations: ['fuzzSuite'] }),
	workload: Object.freeze({ operations: ['workload'], runner_modes: ['runtime-backed'] }),
	'request-rest-route': Object.freeze({ operations: ['workload'], capabilities: ['rest'], runner_modes: ['runtime-backed'] }),
	'request-admin-page': Object.freeze({ operations: ['workload'], capabilities: ['admin'], runner_modes: ['runtime-backed'] }),
	'exercise-admin-page-read-only-interaction': Object.freeze({ operations: ['workload'], capabilities: ['admin'], runner_modes: ['runtime-backed'] }),
	'plan-admin-page-mutation': Object.freeze({ operations: ['workload'], capabilities: ['admin', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES], runner_modes: ['runtime-backed'] }),
	'exercise-ajax-action': Object.freeze({ operations: ['workload'], capabilities: ['admin'], runner_modes: ['runtime-backed'] }),
	'render-block': Object.freeze({ operations: ['workload'], capabilities: ['browser'], runner_modes: ['runtime-backed'] }),
	'serialize-parse-block': Object.freeze({ operations: ['workload'] }),
	'insert-block-in-editor': Object.freeze({ operations: ['workload'], capabilities: ['browser'], runner_modes: ['runtime-backed'] }),
	'inspect-database-table': Object.freeze({ operations: ['workload'], capabilities: ['database'] }),
	'profile-database-query': Object.freeze({ operations: ['workload'], capabilities: ['database'] }),
	'mutate-database-table': Object.freeze({ operations: ['workload'], capabilities: ['database', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'mutate-database-query': Object.freeze({ operations: ['workload'], capabilities: ['database', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'list-posts': Object.freeze({ operations: ['workload'], capabilities: ['crud'] }),
	'read-post': Object.freeze({ operations: ['workload'], capabilities: ['crud'] }),
	'create-post': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'update-post': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'delete-post': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'list-users': Object.freeze({ operations: ['workload'], capabilities: ['crud'] }),
	'read-user': Object.freeze({ operations: ['workload'], capabilities: ['crud'] }),
	'create-user': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'update-user': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'delete-user': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'read-option': Object.freeze({ operations: ['workload'], capabilities: ['crud'] }),
	'create-option': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'update-option': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
	'delete-option': Object.freeze({ operations: ['workload'], capabilities: ['crud', ...DISPOSABLE_DESTRUCTIVE_CAPABILITIES] }),
});

function runtimeContracts(manifest = {}) {
	const runtime = manifest?.wordpressRuntime || manifest;
	return {
		commands: runtime?.commands?.wordpressRuntime || runtime?.commands || {},
		abilities: runtime?.abilities?.wordpressRuntime || runtime?.abilities || {},
		runner_modes: runtime?.capabilities?.wordpressRuntime?.runner_modes || runtime?.capabilities?.runner_modes || {},
	};
}

function buildWordPressFuzzCommandManifest(manifest = {}) {
	const runtime = runtimeContracts(manifest);
	return {
		schema: WORDPRESS_FUZZ_COMMAND_MANIFEST_SCHEMA,
		wp_codebox: {
			public_commands: unique(Object.values(runtime.commands)),
			abilities: { ...runtime.abilities },
			runner_modes: Object.keys(runtime.runner_modes).filter((mode) => runtime.runner_modes[mode] === true).sort(),
		},
		case_intents: Object.fromEntries(Object.entries(CASE_INTENT_REQUIREMENTS).map(([intent, requirements]) => [intent, resolveRequirements(requirements, runtime)])),
	};
}

function requiredWpCodeboxContractsForFuzzPlan(plan = {}, manifest = {}) {
	return mergeRequirements([
		requiredWpCodeboxContractsForFuzzCase({}, manifest),
		...arrayOf(plan.targets).flatMap((target) => arrayOf(target.cases).map((testCase) => requiredWpCodeboxContractsForFuzzCase(testCase, manifest))),
	]);
}

function requiredWpCodeboxContractsForFuzzCase(testCase = {}, manifest = {}) {
	const intent = String(testCase.intent || '').trim();
	const requirements = resolveRequirements(CASE_INTENT_REQUIREMENTS[intent] || CASE_INTENT_REQUIREMENTS.workload, runtimeContracts(manifest));
	return mergeRequirements([requirements, { capabilities: arrayOf(testCase.required_capabilities || testCase.requiredCapabilities || testCase.metadata?.required_capabilities || testCase.metadata?.requiredCapabilities) }]);
}

function resolveRequirements(requirements, runtime) {
	const commandFor = { fuzzSuite: runtime.commands.runFuzzSuite, workload: runtime.commands.runWorkload };
	const abilityFor = { fuzzSuite: runtime.abilities.runFuzzSuite, workload: runtime.abilities.runWorkload };
	return { commands: unique(arrayOf(requirements.operations).map((operation) => commandFor[operation])), abilities: unique(arrayOf(requirements.operations).map((operation) => abilityFor[operation])), capabilities: arrayOf(requirements.capabilities), runner_modes: arrayOf(requirements.runner_modes) };
}

function mergeRequirements(requirements) {
	return { commands: unique(requirements.flatMap((requirement) => arrayOf(requirement.commands))), abilities: unique(requirements.flatMap((requirement) => arrayOf(requirement.abilities))), capabilities: unique(requirements.flatMap((requirement) => arrayOf(requirement.capabilities))), runner_modes: unique(requirements.flatMap((requirement) => arrayOf(requirement.runner_modes || requirement.runnerModes))) };
}

function unique(values) { return [...new Set(values.filter(Boolean))].sort(); }
function arrayOf(value) { return Array.isArray(value) ? value : []; }

module.exports = { WORDPRESS_FUZZ_COMMAND_MANIFEST_SCHEMA, buildWordPressFuzzCommandManifest, requiredWpCodeboxContractsForFuzzCase, requiredWpCodeboxContractsForFuzzPlan };
