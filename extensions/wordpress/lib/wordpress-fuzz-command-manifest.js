'use strict';

const WORDPRESS_FUZZ_COMMAND_MANIFEST_SCHEMA = 'homeboy/wordpress-fuzz-command-manifest/v1';

const WP_CODEBOX_FUZZ_PUBLIC_COMMANDS = Object.freeze([
	'run-fuzz-suite',
	'run-wordpress-workload',
]);

const WP_CODEBOX_FUZZ_PUBLIC_ABILITIES = Object.freeze({
	runFuzzSuite: 'wp-codebox/run-fuzz-suite',
	runWorkload: 'wp-codebox/run-wordpress-workload',
});

const WP_CODEBOX_FUZZ_PUBLIC_RUNNER_MODES = Object.freeze([
	'runtime-backed',
]);

const CASE_INTENT_REQUIREMENTS = Object.freeze({
	default: Object.freeze({ commands: ['run-fuzz-suite'], abilities: ['wp-codebox/run-fuzz-suite'] }),
	workload: Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], runner_modes: ['runtime-backed'] }),
	'request-rest-route': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['rest'], runner_modes: ['runtime-backed'] }),
	'request-admin-page': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['admin'], runner_modes: ['runtime-backed'] }),
	'exercise-admin-page-read-only-interaction': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['admin'], runner_modes: ['runtime-backed'] }),
	'plan-admin-page-mutation': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['admin', 'snapshot', 'restore', 'reset'], runner_modes: ['runtime-backed'] }),
	'exercise-ajax-action': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['admin'], runner_modes: ['runtime-backed'] }),
	'render-block': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['browser'], runner_modes: ['runtime-backed'] }),
	'serialize-parse-block': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'] }),
	'insert-block-in-editor': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['browser'], runner_modes: ['runtime-backed'] }),
	'inspect-database-table': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['database'] }),
	'profile-database-query': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['database'] }),
	'mutate-database-table': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['database', 'snapshot', 'transaction', 'reset'] }),
	'mutate-database-query': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['database', 'snapshot', 'transaction', 'reset'] }),
	'list-posts': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud'] }),
	'read-post': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud'] }),
	'create-post': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'update-post': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'delete-post': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'list-users': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud'] }),
	'read-user': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud'] }),
	'create-user': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'update-user': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'delete-user': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'read-option': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud'] }),
	'create-option': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'update-option': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
	'delete-option': Object.freeze({ commands: ['run-wordpress-workload'], abilities: ['wp-codebox/run-wordpress-workload'], capabilities: ['crud', 'snapshot', 'restore', 'reset'] }),
});

function buildWordPressFuzzCommandManifest() {
	return {
		schema: WORDPRESS_FUZZ_COMMAND_MANIFEST_SCHEMA,
		wp_codebox: {
			public_commands: [...WP_CODEBOX_FUZZ_PUBLIC_COMMANDS],
			abilities: { ...WP_CODEBOX_FUZZ_PUBLIC_ABILITIES },
			runner_modes: [...WP_CODEBOX_FUZZ_PUBLIC_RUNNER_MODES],
		},
		case_intents: Object.fromEntries(Object.entries(CASE_INTENT_REQUIREMENTS).map(([intent, requirements]) => [intent, cloneRequirements(requirements)])),
	};
}

function requiredWpCodeboxContractsForFuzzPlan(plan = {}) {
	const requirements = [CASE_INTENT_REQUIREMENTS.default];
	for (const target of arrayOf(plan.targets)) {
		for (const testCase of arrayOf(target.cases)) {
			requirements.push(requiredWpCodeboxContractsForFuzzCase(testCase));
		}
	}
	return mergeRequirements(requirements);
}

function requiredWpCodeboxContractsForFuzzCase(testCase = {}) {
	const intent = String(testCase.intent || '').trim();
	return cloneRequirements(CASE_INTENT_REQUIREMENTS[intent] || CASE_INTENT_REQUIREMENTS.workload);
}

function mergeRequirements(requirements) {
	return {
		commands: unique(requirements.flatMap((requirement) => arrayOf(requirement.commands))),
		abilities: unique(requirements.flatMap((requirement) => arrayOf(requirement.abilities))),
		capabilities: unique(requirements.flatMap((requirement) => arrayOf(requirement.capabilities))),
		runner_modes: unique(requirements.flatMap((requirement) => arrayOf(requirement.runner_modes || requirement.runnerModes))),
	};
}

function cloneRequirements(requirements = {}) {
	return {
		commands: arrayOf(requirements.commands),
		abilities: arrayOf(requirements.abilities),
		capabilities: arrayOf(requirements.capabilities),
		runner_modes: arrayOf(requirements.runner_modes || requirements.runnerModes),
	};
}

function unique(values) {
	return [...new Set(values.filter(Boolean))].sort();
}

function arrayOf(value) {
	return Array.isArray(value) ? value : [];
}

module.exports = {
	WORDPRESS_FUZZ_COMMAND_MANIFEST_SCHEMA,
	WP_CODEBOX_FUZZ_PUBLIC_ABILITIES,
	WP_CODEBOX_FUZZ_PUBLIC_COMMANDS,
	WP_CODEBOX_FUZZ_PUBLIC_RUNNER_MODES,
	buildWordPressFuzzCommandManifest,
	requiredWpCodeboxContractsForFuzzCase,
	requiredWpCodeboxContractsForFuzzPlan,
};
