'use strict';

/**
 * Internal dependencies
 */
const { normalizeWordPressRuntimeSurfaceDiscovery } = require('./wordpress-runtime-surface-discovery');
const {
	createHomeboyPublicContractEvidence,
	toFinalizationEvidencePolicy,
} = require('./homeboy-public-contract-evidence-adapter');

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
	const publicContractEvidence = createHomeboyPublicContractEvidence({
		changed_public_contracts: changedContracts,
		testing_instructions: input.testing_instructions,
	});

	return {
		schema: WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA,
		type: 'woocommerce-public-contract-discovery',
		compatibility,
		contracts,
		changed_contracts: changedContracts,
		external_usage: {
			role: 'external-usage',
			source: 'woocommerce-marketplace-extension-scan',
			allowed_statuses: ['completed', 'unavailable_manual_review'],
			review_profile: WOOCOMMERCE_REVIEW_PROFILE,
			purpose: 'Scan WooCommerce marketplace and extension usage for the changed public contracts.',
		},
		public_contract_evidence: publicContractEvidence,
		finalization_evidence_policy: toFinalizationEvidencePolicy(publicContractEvidence),
	};
}

function buildWooCommerceCompatibilityEvidencePolicy(input = {}) {
	return toFinalizationEvidencePolicy(createHomeboyPublicContractEvidence({
		changed_public_contracts: [{ id: 'woocommerce-public-contract', summary: 'A WooCommerce public contract changed.' }],
		testing_instructions: input.testing_instructions,
	}));
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
		summary: `${surface.type}: ${value}`,
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
