'use strict';

const WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA = 'homeboy/wordpress-workload-scale-profile/v1';

const WORDPRESS_WORKLOAD_SCALE_DIMENSIONS = Object.freeze([
	'catalog-content-volume',
	'resource-volume',
	'taxonomy-density',
	'meta-density',
	'option-pollution',
	'transient-pollution',
	'queue-backlog',
	'media-volume',
	'account-volume',
	'admin-list-table-scale',
	'rest-collection-scale',
]);

const WORDPRESS_WORKLOAD_SCALE_DIMENSION_ALIASES = new Map([
	['catalog', 'catalog-content-volume'],
	['catalog_content', 'catalog-content-volume'],
	['catalog_content_volume', 'catalog-content-volume'],
	['content_volume', 'catalog-content-volume'],
	['posts_volume', 'catalog-content-volume'],
	['resources', 'resource-volume'],
	['resource_volume', 'resource-volume'],
	['custom_post_volume', 'resource-volume'],
	['records_volume', 'resource-volume'],
	['taxonomy_density', 'taxonomy-density'],
	['terms_density', 'taxonomy-density'],
	['meta_density', 'meta-density'],
	['metadata_density', 'meta-density'],
	['option_pollution', 'option-pollution'],
	['options_pollution', 'option-pollution'],
	['transient_pollution', 'transient-pollution'],
	['transients_pollution', 'transient-pollution'],
	['queue', 'queue-backlog'],
	['queue_backlog', 'queue-backlog'],
	['scheduled_actions', 'queue-backlog'],
	['scheduled_backlog', 'queue-backlog'],
	['media', 'media-volume'],
	['media_volume', 'media-volume'],
	['attachments_volume', 'media-volume'],
	['accounts', 'account-volume'],
	['account_volume', 'account-volume'],
	['users_volume', 'account-volume'],
	['admin_list_table', 'admin-list-table-scale'],
	['admin_list_table_scale', 'admin-list-table-scale'],
	['list_table_scale', 'admin-list-table-scale'],
	['rest_collection', 'rest-collection-scale'],
	['rest_collection_scale', 'rest-collection-scale'],
	['rest_pagination', 'rest-collection-scale'],
]);

const WORDPRESS_WORKLOAD_SCALE_DIMENSION_SET = new Set(WORDPRESS_WORKLOAD_SCALE_DIMENSIONS);

function normalizeWordPressWorkloadScaleProfile(profile) {
	assertPlainObject(profile, 'scale_profile');
	assertSchema(profile.schema, WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA, 'WordPress workload scale profile');
	const id = normalizeId(profile.id, 'wordpress-workload-scale', 'scale_profile.id');
	const rawDimensions = profile.dimensions || profile.scale_dimensions || profile.scaleDimensions || profile.surfaces;
	const dimensions = asArray(rawDimensions, 'scale_profile.dimensions').map(normalizeWordPressWorkloadScaleDimension);

	return stripUndefined({
		schema: WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA,
		id,
		label: profile.label || id,
		dimensions,
		external_values: normalizeExternalValues(profile.external_values || profile.externalValues || profile.product_values || profile.productValues),
		contract_state: dimensions.some((dimension) => dimension.contract_state === 'external-values-required') ? 'external-values-required' : 'declared',
		metadata: objectOrUndefined(profile.metadata) || {},
	});
}

function normalizeWordPressWorkloadScaleDimension(dimension, index) {
	assertPlainObject(dimension, `scale_profile.dimensions[${index}]`);
	const category = normalizeWordPressWorkloadScaleDimensionCategory(dimension.category || dimension.dimension || dimension.type || dimension.kind);
	if (!category) {
		throw new Error(`Unsupported WordPress workload scale dimension: ${dimension.category || dimension.dimension || dimension.type || dimension.kind}`);
	}
	const id = normalizeId(dimension.id, `${category}-${index + 1}`, `scale_profile.dimensions[${index}].id`);
	const values = normalizeDimensionValues(dimension.values || dimension.value || dimension.metrics || dimension.bounds);
	const hasValues = Object.keys(values).length > 0;

	return stripUndefined({
		id,
		category,
		label: dimension.label || id,
		target: normalizeDimensionTarget(dimension.target || dimension.resource || dimension.collection),
		values,
		contract_state: dimension.contract_state || dimension.contractState || (hasValues ? 'declared' : 'external-values-required'),
		executable_state: dimension.executable_state || dimension.executableState || 'plan-only',
		surface_type: dimension.surface_type || dimension.surfaceType || surfaceTypeForScaleDimensionCategory(category),
		metadata: objectOrUndefined(dimension.metadata) || {},
	});
}

function normalizeWordPressWorkloadScaleDimensionCategory(value) {
	const key = String(value || '').trim().toLowerCase().replace(/\s+/g, '-');
	const aliasKey = key.replace(/-/g, '_');
	return WORDPRESS_WORKLOAD_SCALE_DIMENSION_ALIASES.get(key) || WORDPRESS_WORKLOAD_SCALE_DIMENSION_ALIASES.get(aliasKey) || (WORDPRESS_WORKLOAD_SCALE_DIMENSION_SET.has(key) ? key : '');
}

function workloadScaleSurfacesFromProfile(profile) {
	const normalized = normalizeWordPressWorkloadScaleProfile(profile);
	return normalized.dimensions.map((dimension) => ({
		id: `scale:${dimension.id}`,
		type: 'workload-scale',
		label: dimension.label,
		scale_category: dimension.category,
		surface_type: dimension.surface_type,
		contract_state: dimension.contract_state,
		executable_state: dimension.executable_state,
		metadata: {
			workload_scale_dimension: dimension,
		},
	}));
}

function surfaceTypeForScaleDimensionCategory(category) {
	return {
		'catalog-content-volume': 'post-type',
		'resource-volume': 'crud-resource',
		'taxonomy-density': 'taxonomy',
		'meta-density': 'database-table',
		'option-pollution': 'option',
		'transient-pollution': 'option',
		'queue-backlog': 'cron-event',
		'media-volume': 'media',
		'account-volume': 'user',
		'admin-list-table-scale': 'admin-page',
		'rest-collection-scale': 'rest-route',
	}[category] || 'workload-scale';
}

function normalizeDimensionTarget(value) {
	if (value === undefined || value === null) {
		return undefined;
	}
	if (typeof value === 'string') {
		return { id: value };
	}
	assertPlainObject(value, 'scale_dimension.target');
	return { ...value };
}

function normalizeDimensionValues(value) {
	if (value === undefined || value === null) {
		return {};
	}
	if (Number.isFinite(value)) {
		return { count: value };
	}
	assertPlainObject(value, 'scale_dimension.values');
	return { ...value };
}

function normalizeExternalValues(value) {
	if (value === undefined || value === null) {
		return {};
	}
	assertPlainObject(value, 'scale_profile.external_values');
	return { ...value };
}

function normalizeId(value, fallback, field) {
	const id = value || fallback;
	if (!id || typeof id !== 'string') {
		throw new Error(`${field} must be a string.`);
	}
	return id;
}

function asArray(value, field) {
	if (value === undefined || value === null) {
		return [];
	}
	if (!Array.isArray(value)) {
		throw new Error(`${field} must be an array.`);
	}
	return value;
}

function assertPlainObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
}

function assertSchema(value, expected, field) {
	if (value && value !== expected) {
		throw new Error(`Unsupported ${field} schema: ${value}`);
	}
}

function objectOrUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return undefined;
	}
	return { ...value };
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	WORDPRESS_WORKLOAD_SCALE_DIMENSIONS,
	WORDPRESS_WORKLOAD_SCALE_PROFILE_SCHEMA,
	normalizeWordPressWorkloadScaleDimensionCategory,
	normalizeWordPressWorkloadScaleProfile,
	workloadScaleSurfacesFromProfile,
};
