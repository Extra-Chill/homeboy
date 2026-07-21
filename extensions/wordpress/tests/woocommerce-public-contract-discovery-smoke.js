'use strict';

const assert = require('node:assert/strict');

const {
	WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA,
	WOOCOMMERCE_REVIEW_PROFILE,
	buildWooCommerceCompatibilityEvidencePolicy,
	discoverWooCommercePublicContracts,
} = require('../lib/woocommerce-public-contract-discovery');

const input = {
	compatibility: {
		woocommerce: { required: true, version: '9.9.5', source: 'plugin-header' },
	},
	changed_contracts: ['rest:/wc/v3/products'],
	discovery: {
		restRoutes: [
			{ route: '/wc/v3/products', methods: ['GET'], source: 'rest_get_server' },
			{ route: '/wc/store/v1/products', methods: ['GET'], source: 'rest_get_server' },
			{ route: '/wc-admin/options', methods: ['GET'], source: 'rest_get_server', source_path: 'src/Internal.php' },
			{ route: '/wp/v2/posts', methods: ['GET'], source: 'rest_get_server' },
		],
		blocks: [
			{ name: 'woocommerce/cart', source: 'WP_Block_Type_Registry' },
			{ name: 'woocommerce/product-collection', source: 'WP_Block_Type_Registry' },
			{ name: 'example/storefront', source: 'WP_Block_Type_Registry' },
		],
		postTypes: [
			{ name: 'product', public: true, showInRest: true, source: 'get_post_types' },
			{ name: 'product_variation', public: true, showInRest: true, source: 'get_post_types' },
			{ name: 'product_private', public: false, showInRest: true, source: 'get_post_types', source_path: 'src/Internal.php' },
		],
		taxonomies: [
			{ name: 'product_cat', public: true, showInRest: true, source: 'get_taxonomies' },
			{ name: 'product_tag', public: true, showInRest: true, source: 'get_taxonomies' },
			{ name: 'product_visibility', public: false, showInRest: false, source: 'get_taxonomies', source_path: 'src/Internal.php' },
		],
	},
};

const artifact = discoverWooCommercePublicContracts(input);

assert.equal(artifact.schema, WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA);
assert.deepEqual(artifact.compatibility, { plugin: 'woocommerce', version: '9.9.5', source: 'plugin-header' });
assert.deepEqual(artifact.contracts.map((contract) => contract.id), [
	'block:woocommerce/cart',
	'block:woocommerce/product-collection',
	'crud:product',
	'crud:product_cat',
	'crud:product_tag',
	'crud:product_variation',
	'rest:/wc/store/v1/products',
	'rest:/wc/v3/products',
]);
assert.equal(artifact.contracts.some((contract) => contract.identifier.includes('wc-admin') || contract.identifier.includes('private') || contract.identifier.includes('visibility')), false);
assert.equal(JSON.stringify(artifact).includes('src/Internal.php'), false);
assert.equal(Object.hasOwn(artifact, 'diagnostics'), false);
assert.deepEqual(artifact.usage_evidence.review_profile, WOOCOMMERCE_REVIEW_PROFILE);
assert.deepEqual(artifact.changed_contracts.map((contract) => contract.id), ['rest:/wc/v3/products']);
assert.deepEqual(artifact.finalization_evidence_policy, buildWooCommerceCompatibilityEvidencePolicy());
assert.deepEqual(artifact.finalization_evidence_policy.testing_instructions.map((instruction) => instruction.command), [
	'homeboy build <component>',
	'homeboy test <component> --ci-job wp-codebox-phpunit',
]);
assert.equal(discoverWooCommercePublicContracts({
	compatibility: input.compatibility,
	discovery: input.discovery,
}).finalization_evidence_policy, null);

assert.throws(
	() => discoverWooCommercePublicContracts({ discovery: input.discovery }),
	/compatibility\.woocommerce.*required: true.*public version/,
);
assert.throws(
	() => discoverWooCommercePublicContracts({ compatibility: { woocommerce: { required: true } }, discovery: input.discovery }),
	/compatibility\.woocommerce.*public version/,
);

console.log('WooCommerce public contract discovery smoke passed.');
