'use strict';

const WP_CODEBOX_BROWSER_COVERAGE_SCHEMA = 'homeboy/wordpress-wp-codebox-browser-coverage/v1';

function normalizeWpCodeboxBrowserCoveragePrimitive(input = {}) {
	const source = plainObject(input);
	const componentId = stringValue(source.component_id ?? source.componentId);
	const scenarios = arrayValue(source.scenarios).map(normalizeScenario).filter(Boolean);
	const traceCommand = normalizeTraceCommand(source.trace_command ?? source.traceCommand);

	if (!componentId) {
		throw new Error('normalizeWpCodeboxBrowserCoveragePrimitive requires component_id.');
	}
	if (!scenarios.length) {
		throw new Error('normalizeWpCodeboxBrowserCoveragePrimitive requires at least one scenario.');
	}
	if (!traceCommand) {
		throw new Error('normalizeWpCodeboxBrowserCoveragePrimitive requires trace_command.');
	}

	return cleanObject({
		schema: WP_CODEBOX_BROWSER_COVERAGE_SCHEMA,
		component_id: componentId,
		required_file: stringValue(source.required_file ?? source.requiredFile),
		activation_file: stringValue(source.activation_file ?? source.activationFile),
		scenarios,
		profile: normalizeProfile(source.profile),
		profile_metadata: normalizeMetadata(source.profile_metadata ?? source.profileMetadata ?? source.metadata),
		trace_command: traceCommand,
	});
}

function normalizeScenario(input = {}) {
	const source = plainObject(input);
	const id = stringValue(source.id ?? source.scenario_id ?? source.scenarioId);
	if (!id) {
		return null;
	}
	return cleanObject({
		id,
		label: stringValue(source.label),
		steps_file: stringValue(source.steps_file ?? source.stepsFile),
		tags: arrayValue(source.tags).map(stringValue).filter(Boolean),
		metadata: normalizeMetadata(source.metadata),
	});
}

function normalizeProfile(input = {}) {
	const source = plainObject(input);
	return cleanObject({
		wp_version: stringValue(source.wp_version ?? source.wpVersion),
		viewport: stringValue(source.viewport),
		step_timeout: stringValue(source.step_timeout ?? source.stepTimeout),
		timeout: stringValue(source.timeout),
		blueprint_steps: arrayValue(source.blueprint_steps ?? source.blueprintSteps),
		inputs: plainObject(source.inputs),
		assumptions: arrayValue(source.assumptions).map(stringValue).filter(Boolean),
		metadata: normalizeMetadata(source.metadata),
	});
}

function normalizeTraceCommand(input) {
	if (typeof input === 'string') {
		const command = input.trim();
		return command ? { command } : null;
	}
	const source = plainObject(input);
	const command = stringValue(source.command);
	const argv = arrayValue(source.argv ?? source.args).map(stringValue).filter(Boolean);
	if (!command && !argv.length) {
		return null;
	}
	return cleanObject({
		command,
		argv,
		cwd: stringValue(source.cwd),
		env: normalizeMetadata(source.env),
	});
}

function normalizeMetadata(input = {}) {
	return stableObject(plainObject(input));
}

function stableObject(input = {}) {
	return Object.fromEntries(Object.entries(input).sort(([a], [b]) => a.localeCompare(b)));
}

function cleanObject(input = {}) {
	return Object.fromEntries(Object.entries(input).filter(([, value]) => {
		if (Array.isArray(value)) {
			return value.length > 0;
		}
		if (value && typeof value === 'object') {
			return Object.keys(value).length > 0;
		}
		return value !== undefined && value !== '' && value !== null;
	}));
}

function plainObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function arrayValue(value) {
	return Array.isArray(value) ? value : [];
}

function stringValue(value) {
	return typeof value === 'string' ? value.trim() : '';
}

module.exports = {
	WP_CODEBOX_BROWSER_COVERAGE_SCHEMA,
	normalizeWpCodeboxBrowserCoveragePrimitive,
};
