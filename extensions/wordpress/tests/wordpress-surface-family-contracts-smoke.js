'use strict';

const assert = require('node:assert/strict');

const fixture = require('./fixtures/wordpress-surface-family-inventory.json');
const {
	buildWordPressFuzzPlanFromSurfaces,
} = require('../lib/wordpress-fuzz-plan-from-surfaces');
const {
	normalizeWordPressRuntimeSurfaceDiscovery,
} = require('../lib/wordpress-runtime-surface-discovery');
const {
	WORDPRESS_SURFACE_EXECUTABLE_STATES,
	WORDPRESS_SURFACE_FAMILIES,
	normalizeWordPressSurfaceFamilyContracts,
} = require('../lib/wordpress-surface-family-contracts');

assert.deepEqual(WORDPRESS_SURFACE_EXECUTABLE_STATES, [
	'read_only_executable',
	'isolated_mutating_executable',
	'discovered',
	'unsupported',
]);
assert.deepEqual(WORDPRESS_SURFACE_FAMILIES.map((family) => family.id), [
	'rest',
	'crud',
	'admin',
	'frontend',
	'blocks-editor',
	'database',
	'wp-cli',
	'hooks-cron',
	'options-settings',
	'users-roles-media-taxonomies',
]);

const plan = buildWordPressFuzzPlanFromSurfaces(fixture, {
	mutation_mode: 'isolated',
	runtimeCapabilities: { capabilities: ['browser', 'block', 'checkpoint', 'reset', 'restore', 'rest', 'rest-rollback', 'external-http-guardrail'] },
});
const contracts = plan.metadata.surface_family_contracts;
assert.equal(contracts.schema, 'homeboy/wordpress-surface-family-contracts/v1');
assert.equal(contracts.families.length, 10);
assert.equal(contracts.metadata.state_counts.read_only_executable > 0, true);
assert.equal(contracts.metadata.state_counts.isolated_mutating_executable > 0, true);
assert.equal(contracts.metadata.state_counts.unsupported > 0, true);

const byFamily = Object.fromEntries(contracts.families.map((family) => [family.id, family]));
assert.deepEqual(byFamily.rest.surface_types, ['rest-route']);
assert.equal(byFamily.rest.cases.some((testCase) => testCase.state === 'read_only_executable'), true);
assert.equal(byFamily.rest.cases.some((testCase) => testCase.state === 'isolated_mutating_executable'), true);
assert.equal(byFamily.database.cases.some((testCase) => testCase.intent === 'mutate-database-table' && testCase.state === 'unsupported'), true);
assert.equal(byFamily['blocks-editor'].cases.some((testCase) => testCase.intent === 'insert-block-in-editor' && testCase.state === 'unsupported'), true);
assert.equal(byFamily['wp-cli'].surfaces[0].state, 'discovered');
assert.equal(byFamily['hooks-cron'].surfaces.length, 2);
assert.equal(byFamily['options-settings'].surfaces.length, 2);
assert.equal(byFamily['users-roles-media-taxonomies'].surfaces.length, 4);
assert(!JSON.stringify(contracts).includes('woocommerce'));
assert(!JSON.stringify(contracts).includes('jetpack'));
assert(!JSON.stringify(contracts).includes('gutenberg'));

const runtimeDiscovery = normalizeWordPressRuntimeSurfaceDiscovery({
	id: 'runtime-fixture-inventory',
	surfaces: fixture.surfaces,
});
const runtimeContracts = normalizeWordPressSurfaceFamilyContracts(runtimeDiscovery);
const runtimeByFamily = Object.fromEntries(runtimeContracts.families.map((family) => [family.id, family]));
assert.equal(runtimeByFamily.rest.surfaces[0].state, 'read_only_executable');
assert.equal(runtimeByFamily.frontend.surfaces[0].state, 'read_only_executable');
assert.equal(runtimeByFamily['wp-cli'].surfaces[0].state, 'discovered');
assert.equal(runtimeByFamily['wp-cli'].state, 'discovered');
assert.equal(runtimeByFamily['hooks-cron'].surfaces.every((surface) => surface.state === 'discovered'), true);
assert.equal(runtimeByFamily['options-settings'].surfaces.every((surface) => surface.state === 'discovered'), true);
assert.equal(runtimeContracts.metadata.state_counts.discovered > 0, true);

const explicitUnsupported = normalizeWordPressSurfaceFamilyContracts({
	surfaces: [{ id: 'db-query:custom', type: 'db-query', execution_state: 'unsupported' }],
});
assert.equal(explicitUnsupported.families.find((family) => family.id === 'database').surfaces[0].state, 'unsupported');
