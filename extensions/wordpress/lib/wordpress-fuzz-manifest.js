'use strict';

/**
 * Internal dependencies
 */
const {
	normalizeWordPressWorkloadProfile,
	workflowInputsFromWordPressWorkloadProfile,
} = require('./wordpress-workload-profile');
const {
	normalizeWordPressSurfaceDiscovery,
	normalizeWordPressFuzzPlan,
} = require('./wordpress-fuzz-schemas');

const WORDPRESS_FUZZ_MANIFEST_SCHEMA = 'homeboy/wordpress-fuzz-manifest/v1';

function assertPlainObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
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

function normalizeId(value, fallback, field) {
	const id = value || fallback;
	if (!id || typeof id !== 'string') {
		throw new Error(`${field} must be a string.`);
	}
	return id;
}

function profileInputFromManifest(manifest, id, label) {
	const profile = manifest.workload_profile || manifest.workloadProfile || {};
	assertPlainObject(profile, 'workload_profile');

	return {
		...profile,
		id: profile.id || manifest.workload_id || manifest.workloadId || id,
		label: profile.label || label,
		dependencies: profile.dependencies || manifest.dependencies,
		wp_config_defines: profile.wp_config_defines || profile.wpConfigDefines || manifest.wp_config_defines || manifest.wpConfigDefines,
		mounts: profile.mounts || manifest.mounts,
		run_before: profile.run_before || profile.runBefore || manifest.run_before || manifest.runBefore,
		workloads: profile.workloads || manifest.workloads,
		run_after: profile.run_after || profile.runAfter || manifest.run_after || manifest.runAfter,
		visual_comparisons: profile.visual_comparisons || profile.visualComparisons || manifest.visual_comparisons || manifest.visualComparisons,
		metadata: {
			...(manifest.metadata || {}),
			...(profile.metadata || {}),
		},
	};
}

function normalizeWordPressFuzzManifest(manifest) {
	assertPlainObject(manifest, 'manifest');
	if (manifest.schema && manifest.schema !== WORDPRESS_FUZZ_MANIFEST_SCHEMA) {
		throw new Error(`Unsupported WordPress fuzz manifest schema: ${manifest.schema}`);
	}

	const id = normalizeId(manifest.id, 'wordpress-fuzz-manifest', 'manifest.id');
	const label = manifest.label || id;
	const workloadProfile = normalizeWordPressWorkloadProfile(profileInputFromManifest(manifest, id, label));

	return {
		schema: WORDPRESS_FUZZ_MANIFEST_SCHEMA,
		id,
		label,
		workload_profile: workloadProfile,
		discovery: manifest.discovery ? normalizeWordPressSurfaceDiscovery(manifest.discovery) : null,
		plan: manifest.plan ? normalizeWordPressFuzzPlan(manifest.plan) : null,
		artifacts: asArray(manifest.artifacts, 'artifacts'),
		budget: { ...(manifest.budget || {}) },
		metadata: { ...(manifest.metadata || {}) },
	};
}

function workflowInputsFromWordPressFuzzManifest(manifest) {
	const normalized = normalizeWordPressFuzzManifest(manifest);
	const workloadInputs = workflowInputsFromWordPressWorkloadProfile(normalized.workload_profile);
	return {
		...workloadInputs,
		wordpress_fuzz_manifest: JSON.stringify(normalized),
		wordpress_fuzz_discovery: JSON.stringify(normalized.discovery),
		wordpress_fuzz_plan: JSON.stringify(normalized.plan),
		wordpress_fuzz_artifacts: JSON.stringify(normalized.artifacts),
		metadata: {
			...workloadInputs.metadata,
			manifest_id: normalized.id,
			manifest_label: normalized.label,
			...normalized.metadata,
		},
	};
}

module.exports = {
	WORDPRESS_FUZZ_MANIFEST_SCHEMA,
	normalizeWordPressFuzzManifest,
	workflowInputsFromWordPressFuzzManifest,
};
