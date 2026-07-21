'use strict';

/**
 * Internal dependencies
 */
const { normalizeWordPressRuntimeSurfaceDiscovery } = require('./wordpress-runtime-surface-discovery');

const WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA = 'homeboy/woocommerce-public-contract-discovery/v1';
const WOOCOMMERCE_REVIEW_PROFILE = Object.freeze({
	id: 'wp-codebox-validation',
	jobs: Object.freeze(['wp-codebox-build-smoke', 'wp-codebox-phpunit', 'wp-codebox-bench-offloaded']),
});

function discoverWooCommercePublicContracts(input = {}) {
	const compatibility = requiredWooCommerceCompatibility(input.compatibility);
	const discovery = input.discovery?.schema === 'homeboy/wordpress-surface-discovery/v1'
		? input.discovery
		: normalizeWordPressRuntimeSurfaceDiscovery(input.discovery || input);
	const contracts = discovery.surfaces
		.filter(isWooCommercePublicSurface)
		.map((surface) => publicContract(surface))
		.sort((left, right) => left.id.localeCompare(right.id));
	const changedContracts = changedWooCommercePublicContracts(input, contracts);

	return {
		schema: WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA,
		type: 'woocommerce-public-contract-discovery',
		compatibility,
		contracts,
		changed_contracts: changedContracts,
		usage_evidence: {
			review_profile: WOOCOMMERCE_REVIEW_PROFILE,
			purpose: 'Validate the declared WooCommerce public contracts with the WordPress extension review profile.',
		},
		finalization_evidence_policy: changedContracts.length > 0 ? buildWooCommerceCompatibilityEvidencePolicy() : null,
	};
}

function buildWooCommerceCompatibilityEvidencePolicy() {
	return {
		schema: 'homeboy/finalization-evidence-policy/v1',
		required_evidence: [
			{
				id: 'woocommerce-backwards-compatibility-impact',
				reviewer_facing: true,
				durable_url_required: true,
				fields: [{ name: 'statement', min_length: 1 }, { name: 'external_consumers', min_length: 1 }],
			},
			{
				id: 'woocommerce-extension-usage-scan',
				reviewer_facing: true,
				durable_url_required: true,
				fields: [
					{ name: 'status', values: ['completed', 'unavailable_manual_review'] },
					{ name: 'source', min_length: 1 },
					{ name: 'limitations', min_length: 1 },
				],
			},
		],
		testing_instructions: [
			{ command: 'homeboy build <component>' },
			{ command: 'homeboy test <component> --ci-job wp-codebox-phpunit' },
		],
	};
}

function changedWooCommercePublicContracts(input, contracts) {
	const changedIds = Array.isArray(input.changed_contracts || input.changedContracts)
		? (input.changed_contracts || input.changedContracts)
			.map((entry) => typeof entry === 'string' ? entry : entry?.id)
			.filter((id) => typeof id === 'string')
		: [];
	return contracts.filter((contract) => changedIds.includes(contract.id));
}

function requiredWooCommerceCompatibility(value) {
	const declaration = value?.woocommerce;
	if (declaration?.required !== true || !stringValue(declaration.version)) {
		throw new Error('WooCommerce public contract discovery requires compatibility.woocommerce with required: true and a public version.');
	}
	return {
		plugin: 'woocommerce',
		version: stringValue(declaration.version),
		source: stringValue(declaration.source, 'plugin-header'),
	};
}

function isWooCommercePublicSurface(surface) {
	const value = stringValue(surface.metadata?.value);
	if (surface.type === 'rest_route') {
		return /^\/wc\/(?:v\d+|store\/v\d+)(?:\/|$)/.test(value);
	}
	if (surface.type === 'block') {
		return /^woocommerce\/[a-z0-9-]+$/.test(value);
	}
	if (surface.type !== 'crud_resource' || surface.metadata?.public !== true || surface.metadata?.show_in_rest !== true) {
		return false;
	}
	if (surface.metadata.source_type === 'post_type') {
		return value === 'product' || value === 'product_variation';
	}
	return surface.metadata.source_type === 'taxonomy' && (value === 'product_cat' || value === 'product_tag');
}

function publicContract(surface) {
	const value = stringValue(surface.metadata.value);
	return {
		id: surface.id,
		type: surface.type,
		identifier: value,
		workload: surface.workload,
		evidence: {
			source: surface.metadata.source,
			public: surface.metadata.public,
			show_in_rest: surface.metadata.show_in_rest,
		},
	};
}

function stringValue(value, fallback = '') {
	return typeof value === 'string' && value.trim() ? value.trim() : fallback;
}

module.exports = {
	WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA,
	WOOCOMMERCE_REVIEW_PROFILE,
	buildWooCommerceCompatibilityEvidencePolicy,
	discoverWooCommercePublicContracts,
};
