'use strict';

const assert = require('node:assert/strict');

const {
	WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA,
	WOOCOMMERCE_REVIEW_PROFILE,
	discoverWooCommercePublicContracts,
} = require('../lib/woocommerce-public-contract-discovery');
const {
	HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA,
	toFinalizationEvidencePolicy,
} = require('../lib/homeboy-public-contract-evidence-adapter');

const input = {
	compatibility: {
		woocommerce: { required: true, version: '9.9.5', source: 'plugin-header' },
	},
	changed_contracts: ['rest:/wc/v3/products'],
	testing_instructions: [
		{ number: 1, command: 'pnpm --dir plugins/woocommerce test:phpunit -- tests/api/products' },
		{ number: 2, command: 'pnpm --dir plugins/woocommerce test:e2e -- specs/cart.spec.js' },
	],
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
assert.deepEqual(artifact.external_usage.review_profile, WOOCOMMERCE_REVIEW_PROFILE);
assert.equal(artifact.external_usage.role, 'external-usage');
assert.equal(artifact.external_usage.source, 'woocommerce-marketplace-extension-scan');
assert.deepEqual(artifact.external_usage.allowed_statuses, ['completed', 'unavailable_manual_review']);
assert.deepEqual(artifact.changed_contracts.map((contract) => contract.id), ['rest:/wc/v3/products']);
assert.equal(artifact.changed_contracts[0].summary, 'rest_route: /wc/v3/products');
assert.equal(artifact.public_contract_evidence.schema, HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA);
assert.deepEqual(artifact.public_contract_evidence.changed_public_contracts, artifact.changed_contracts);
assert.deepEqual(artifact.public_contract_evidence.required_evidence.map((evidence) => evidence.role), [
	'compatibility-impact',
	'external-consumer-impact',
	'external-usage',
]);
assert.deepEqual(artifact.finalization_evidence_policy, toFinalizationEvidencePolicy(artifact.public_contract_evidence));
assert.deepEqual(artifact.finalization_evidence_policy.testing_instructions.map((instruction) => instruction.command), [
	'pnpm --dir plugins/woocommerce test:phpunit -- tests/api/products',
	'pnpm --dir plugins/woocommerce test:e2e -- specs/cart.spec.js',
]);
assert.deepEqual(artifact.finalization_evidence_policy.testing_instructions.map((instruction) => instruction.number), [1, 2]);
assert.deepEqual(discoverWooCommercePublicContracts({
	compatibility: input.compatibility,
	changed_contracts: input.changed_contracts,
	discovery: input.discovery,
}).finalization_evidence_policy.testing_instructions, []);
assert.equal(discoverWooCommercePublicContracts({
	compatibility: input.compatibility,
	discovery: input.discovery,
}).public_contract_evidence, null);

assert.throws(
	() => discoverWooCommercePublicContracts({ discovery: input.discovery }),
	/compatibility\.woocommerce.*required: true.*public version/,
);
assert.throws(
	() => discoverWooCommercePublicContracts({ compatibility: { woocommerce: { required: true } }, discovery: input.discovery }),
	/compatibility\.woocommerce.*public version/,
);

console.log('WooCommerce public contract discovery smoke passed.');
