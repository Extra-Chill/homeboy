'use strict';

const assert = require('node:assert/strict');
const {
	WP_CODEBOX_BROWSER_COVERAGE_SCHEMA,
	normalizeWpCodeboxBrowserCoveragePrimitive,
} = require('../lib/wp-codebox-browser-coverage');

const declaration = normalizeWpCodeboxBrowserCoveragePrimitive({
	componentId: 'woocommerce',
	requiredFile: 'woocommerce.php',
	activationFile: 'woocommerce.php',
	scenarios: [
		{
			id: 'shop',
			label: 'Shop page',
			stepsFile: 'browser-scenarios/shop.json',
			tags: ['frontend', ' request-coverage '],
			metadata: { route: '/shop/' },
		},
	],
	profile: {
		wpVersion: '7.0',
		viewport: '1366x900',
		stepTimeout: '45s',
		timeout: '180s',
		blueprintSteps: [{ step: 'login', username: 'admin', password: 'password' }],
		inputs: { pluginRuntime: { mode: 'local' } },
		assumptions: ['logged-in admin'],
	},
	profileMetadata: { owner: 'fuzz', coverage: 'browser-request' },
	traceCommand: {
		command: 'node',
		args: ['bench/woocommerce-browser-coverage.trace.mjs'],
		cwd: '${component.root}',
		env: { HOMEBOY_TRACE_SCENARIO: 'shop' },
	},
});

assert.equal(declaration.schema, WP_CODEBOX_BROWSER_COVERAGE_SCHEMA);
assert.equal(declaration.component_id, 'woocommerce');
assert.equal(declaration.required_file, 'woocommerce.php');
assert.equal(declaration.activation_file, 'woocommerce.php');
assert.deepEqual(declaration.scenarios, [
	{
		id: 'shop',
		label: 'Shop page',
		steps_file: 'browser-scenarios/shop.json',
		tags: ['frontend', 'request-coverage'],
		metadata: { route: '/shop/' },
	},
]);
assert.deepEqual(declaration.profile, {
	wp_version: '7.0',
	viewport: '1366x900',
	step_timeout: '45s',
	timeout: '180s',
	blueprint_steps: [{ step: 'login', username: 'admin', password: 'password' }],
	inputs: { pluginRuntime: { mode: 'local' } },
	assumptions: ['logged-in admin'],
});
assert.deepEqual(declaration.profile_metadata, { coverage: 'browser-request', owner: 'fuzz' });
assert.deepEqual(declaration.trace_command, {
	command: 'node',
	argv: ['bench/woocommerce-browser-coverage.trace.mjs'],
	cwd: '${component.root}',
	env: { HOMEBOY_TRACE_SCENARIO: 'shop' },
});

assert.deepEqual(normalizeWpCodeboxBrowserCoveragePrimitive({
	component_id: 'jetpack',
	scenarios: [{ scenario_id: 'dashboard' }],
	trace_command: 'node bench/jetpack-browser-coverage.trace.mjs',
}).trace_command, { command: 'node bench/jetpack-browser-coverage.trace.mjs' });

assert.throws(
	() => normalizeWpCodeboxBrowserCoveragePrimitive({ scenarios: [{ id: 'shop' }], trace_command: 'node trace.mjs' }),
	/requires component_id/
);
assert.throws(
	() => normalizeWpCodeboxBrowserCoveragePrimitive({ component_id: 'woocommerce', trace_command: 'node trace.mjs' }),
	/requires at least one scenario/
);
assert.throws(
	() => normalizeWpCodeboxBrowserCoveragePrimitive({ component_id: 'woocommerce', scenarios: [{ id: 'shop' }] }),
	/requires trace_command/
);

console.log('WP Codebox browser coverage primitive smoke passed.');
