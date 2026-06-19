'use strict';

const WORKLOAD_PROFILE_SCHEMA = 'homeboy/wordpress-workload-profile/v1';

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

function normalizeStep(step, field) {
	assertPlainObject(step, field);
	if (!step.type || typeof step.type !== 'string') {
		throw new Error(`${field}.type must be a string.`);
	}

	return { ...step };
}

function normalizeDependency(dependency, index) {
	if (typeof dependency === 'string') {
		return { entry: dependency };
	}

	assertPlainObject(dependency, `dependencies[${index}]`);
	let entry = dependency.entry || dependency.repo || dependency.component || dependency.path;
	if (!entry || typeof entry !== 'string') {
		throw new Error(`dependencies[${index}] requires entry, repo, component, or path.`);
	}
	if (dependency.ref && !entry.includes('@')) {
		entry = `${entry}@${dependency.ref}`;
	}

	return { ...dependency, entry };
}

function normalizeMount(mount, index) {
	if (typeof mount === 'string') {
		const parts = mount.split(':');
		if (parts.length < 2) {
			throw new Error(`mounts[${index}] string must use source:target[:mode].`);
		}
		const [source, target, mode] = parts;
		return { source, target, ...(mode ? { mode } : {}) };
	}

	assertPlainObject(mount, `mounts[${index}]`);
	if (!mount.source || !mount.target) {
		throw new Error(`mounts[${index}] requires source and target.`);
	}

	return { ...mount };
}

function normalizeVisualComparison(comparison, index) {
	assertPlainObject(comparison, `visual_comparisons[${index}]`);
	const id = comparison.id || comparison.name || `visual-comparison-${index + 1}`;
	if (typeof id !== 'string') {
		throw new Error(`visual_comparisons[${index}].id must be a string.`);
	}

	return {
		...comparison,
		id,
		artifacts_directory: comparison.artifacts_directory || comparison.artifactsDirectory || `artifacts/visual/${id}`,
	};
}

function normalizeWorkload(workload, index) {
	assertPlainObject(workload, `workloads[${index}]`);
	if (!workload.id || typeof workload.id !== 'string') {
		throw new Error(`workloads[${index}].id must be a string.`);
	}

	const run = asArray(workload.run || workload.steps, `workloads[${index}].run`).map((step, stepIndex) => (
		normalizeStep(step, `workloads[${index}].run[${stepIndex}]`)
	));
	if (run.length === 0) {
		throw new Error(`workloads[${index}].run must contain at least one step.`);
	}

	return {
		...workload,
		run,
	};
}

function normalizeWordPressWorkloadProfile(profile) {
	assertPlainObject(profile, 'profile');
	if (profile.schema && profile.schema !== WORKLOAD_PROFILE_SCHEMA) {
		throw new Error(`Unsupported WordPress workload profile schema: ${profile.schema}`);
	}
	if (!profile.id || typeof profile.id !== 'string') {
		throw new Error('profile.id must be a string.');
	}

	return {
		schema: WORKLOAD_PROFILE_SCHEMA,
		id: profile.id,
		label: profile.label || profile.id,
		dependencies: asArray(profile.dependencies, 'dependencies').map(normalizeDependency),
		wp_config_defines: { ...(profile.wp_config_defines || profile.wpConfigDefines || {}) },
		mounts: asArray(profile.mounts, 'mounts').map(normalizeMount),
		run_before: asArray(profile.run_before || profile.runBefore, 'run_before').map((step, index) => normalizeStep(step, `run_before[${index}]`)),
		workloads: asArray(profile.workloads, 'workloads').map(normalizeWorkload),
		run_after: asArray(profile.run_after || profile.runAfter, 'run_after').map((step, index) => normalizeStep(step, `run_after[${index}]`)),
		visual_comparisons: asArray(profile.visual_comparisons || profile.visualComparisons, 'visual_comparisons').map(normalizeVisualComparison),
		metadata: { ...(profile.metadata || {}) },
	};
}

function workflowInputsFromWordPressWorkloadProfile(profile) {
	const normalized = normalizeWordPressWorkloadProfile(profile);
	const runAfter = [...normalized.run_after];

	for (const comparison of normalized.visual_comparisons) {
		runAfter.push({
			type: 'visual-compare',
			...comparison,
		});
	}

	return {
		validation_dependencies: normalized.dependencies.map((dependency) => dependency.entry).join(','),
		extra_wp_config_defines: JSON.stringify(normalized.wp_config_defines),
		runtime_mounts: JSON.stringify(normalized.mounts),
		workload_run_before: JSON.stringify(normalized.run_before),
		wordpress_runtime_workloads: JSON.stringify(normalized.workloads),
		workload_run_after: JSON.stringify(runAfter),
		metadata: {
			profile_id: normalized.id,
			profile_label: normalized.label,
			...normalized.metadata,
		},
	};
}

module.exports = {
	WORKLOAD_PROFILE_SCHEMA,
	normalizeWordPressWorkloadProfile,
	workflowInputsFromWordPressWorkloadProfile,
};
