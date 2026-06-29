'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_WORKLOAD_SCALE_DIMENSIONS,
	WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA,
	normalizeWordPressWorkloadScaleDimensionCategory,
	normalizeWordPressWorkloadScaleProfile,
	workloadScaleSurfacesFromProfile,
} = require('../lib/wordpress-workload-scale-profile');
const {
	buildWordPressFuzzPlanFromSurfaces,
	collectWordPressFuzzPlanSurfaces,
} = require('../lib/wordpress-fuzz-plan-from-surfaces');

const scaleProfile = normalizeWordPressWorkloadScaleProfile({
	schema: WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA,
	id: 'large-generic-site',
	dimensions: [
		{ id: 'catalog-items', category: 'catalog_content_volume', target: { post_type: 'item' }, values: { count: 100000 } },
		{ id: 'resource-records', category: 'custom_post_volume', target: { post_type: 'transaction_record' }, values: { count: 250000 } },
		{ id: 'terms', category: 'taxonomy_density', target: { taxonomy: 'topic' }, values: { terms: 5000, terms_per_object: 20 } },
		{ id: 'object-meta', category: 'meta_density', values: { rows: 750000, keys_per_object: 30 } },
		{ id: 'options', category: 'option_pollution', values: { rows: 20000, autoloaded_rows: 1000 } },
		{ id: 'transients', category: 'transient_pollution', values: { rows: 50000, expired_ratio: 0.4 } },
		{ id: 'queue', category: 'queue_backlog', values: { pending: 40000, failed: 300 } },
		{ id: 'media', category: 'media_volume', values: { attachments: 60000 } },
		{ id: 'accounts', category: 'account_volume', values: { users: 120000 } },
		{ id: 'admin-list', category: 'admin_list_table_scale', target: { path: '/wp-admin/edit.php' }, values: { rows: 100000, page_size: 999 } },
		{ id: 'rest-collection', category: 'rest_collection_scale', target: { route: '/wp/v2/items' }, values: { per_page: 100, total_items: 100000, filters: 12, search_terms: 50 } },
		{ id: 'external-resource', category: 'resource-volume' },
	],
	product_values: {
		provided_by: 'downstream-rig',
		fixture_ref: 'artifact://product-scale-values.json',
	},
});

assert.equal(scaleProfile.schema, WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA);
assert.deepEqual(scaleProfile.dimensions.map((dimension) => dimension.category), WORDPRESS_WORKLOAD_SCALE_DIMENSIONS.concat(['resource-volume']));
assert.equal(scaleProfile.dimensions[0].surface_type, 'post-type');
assert.equal(scaleProfile.dimensions[1].surface_type, 'crud-resource');
assert.equal(scaleProfile.dimensions[2].surface_type, 'taxonomy');
assert.equal(scaleProfile.dimensions[3].surface_type, 'database-table');
assert.equal(scaleProfile.dimensions[4].surface_type, 'option');
assert.equal(scaleProfile.dimensions[5].surface_type, 'option');
assert.equal(scaleProfile.dimensions[6].surface_type, 'cron-event');
assert.equal(scaleProfile.dimensions[7].surface_type, 'media');
assert.equal(scaleProfile.dimensions[8].surface_type, 'user');
assert.equal(scaleProfile.dimensions[9].surface_type, 'admin-page');
assert.equal(scaleProfile.dimensions[10].surface_type, 'rest-route');
assert.equal(scaleProfile.dimensions[11].contract_state, 'external-values-required');
assert.equal(scaleProfile.contract_state, 'external-values-required');
assert.deepEqual(scaleProfile.external_values, { provided_by: 'downstream-rig', fixture_ref: 'artifact://product-scale-values.json' });
assert.equal(normalizeWordPressWorkloadScaleDimensionCategory('scheduled_actions'), 'queue-backlog');
assert.throws(() => normalizeWordPressWorkloadScaleProfile({ dimensions: [{ category: 'vendor-specific-resource' }] }), /Unsupported WordPress workload scale dimension/);

const surfaces = workloadScaleSurfacesFromProfile(scaleProfile);
assert.equal(surfaces.length, 12);
assert.equal(surfaces[0].type, 'workload-scale');
assert.equal(surfaces[0].scale_category, 'catalog-content-volume');
assert.equal(surfaces[11].contract_state, 'external-values-required');

const collectedSurfaces = collectWordPressFuzzPlanSurfaces({ workload_scale_profile: scaleProfile });
assert.equal(collectedSurfaces.length, 12);

const plan = buildWordPressFuzzPlanFromSurfaces({ id: 'scale-site', workload_scale_profile: scaleProfile });
assert.equal(plan.metadata.workload_scale_profile.id, 'large-generic-site');
assert.equal(plan.targets.length, 12);
assert(plan.targets.every((target) => target.type === 'workload-scale'));
assert(plan.targets.every((target) => target.cases[0].execution_tier === 'plan_only'));
assert(plan.targets.every((target) => target.cases[0].executable === false));
assert(plan.targets.every((target) => target.cases[0].skip_reasons.includes('declarative-scale-contract')));
assert.equal(plan.targets[0].cases[0].operation.category, 'catalog-content-volume');
assert.equal(plan.targets[0].cases[0].operation.executable_state, 'plan-only');
assert.equal(plan.targets[11].cases[0].operation.contract_state, 'external-values-required');
assert.equal(plan.metadata.execution_tiers.plan_only, 12);
assert.deepEqual(plan.metadata.workload_scale_profile.external_values, { provided_by: 'downstream-rig', fixture_ref: 'artifact://product-scale-values.json' });
assert(!JSON.stringify(plan.targets).includes('product-scale-values'), 'product values remain external to plan targets');

console.log('WordPress workload scale profile smoke passed.');
