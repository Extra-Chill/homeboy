'use strict';

const WORDPRESS_SURFACE_FAMILY_CONTRACT_SCHEMA = 'homeboy/wordpress-surface-family-contracts/v1';

const WORDPRESS_SURFACE_EXECUTABLE_STATES = Object.freeze([
	'read_only_executable',
	'isolated_mutating_executable',
	'discovered',
	'unsupported',
]);

const WORDPRESS_SURFACE_FAMILIES = Object.freeze([
	{ id: 'rest', label: 'REST', types: ['rest-route'] },
	{ id: 'crud', label: 'CRUD', types: ['crud-resource', 'post-type'] },
	{ id: 'admin', label: 'Admin pages/actions', types: ['admin-page', 'ajax-action'] },
	{ id: 'frontend', label: 'Frontend', types: ['frontend-url'] },
	{ id: 'blocks-editor', label: 'Blocks/editor', types: ['block'] },
	{ id: 'database', label: 'DB tables/queries', types: ['database-table', 'db-query'] },
	{ id: 'wp-cli', label: 'WP-CLI', types: ['wp-cli-command'] },
	{ id: 'hooks-cron', label: 'Hooks/cron', types: ['hook', 'cron-event'] },
	{ id: 'options-settings', label: 'Options/settings', types: ['option', 'setting'] },
	{ id: 'users-roles-media-taxonomies', label: 'Users/roles/media/taxonomies', types: ['user', 'role', 'media', 'taxonomy', 'capability'] },
]);

const FAMILY_BY_TYPE = new Map(WORDPRESS_SURFACE_FAMILIES.flatMap((family) => family.types.map((type) => [type, family])));

function normalizeWordPressSurfaceFamilyContracts(input = {}, options = {}) {
	const surfaces = collectContractSurfaces(input);
	const cases = collectContractCases(input);
	const families = WORDPRESS_SURFACE_FAMILIES.map((family) => normalizeFamilyContract(family, surfaces, cases));
	const stateCounts = countStates([
		...families.flatMap((family) => family.surfaces),
		...families.flatMap((family) => family.cases),
	]);

	return {
		schema: WORDPRESS_SURFACE_FAMILY_CONTRACT_SCHEMA,
		id: stringValue(input.id || input.plan_id || input.planId || options.id, 'wordpress-surface-family-contracts'),
		families,
		executable_states: [...WORDPRESS_SURFACE_EXECUTABLE_STATES],
		metadata: {
			...(isObject(input.metadata) ? input.metadata : {}),
			state_counts: stateCounts,
		},
	};
}

function normalizeFamilyContract(family, surfaces, cases) {
	const familySurfaces = surfaces.filter((surface) => family.types.includes(surface.type));
	const familyCases = cases.filter((testCase) => family.types.includes(testCase.surface_type || testCase.type));
	const states = countStates([...familySurfaces, ...familyCases]);
	return {
		id: family.id,
		label: family.label,
		surface_types: [...family.types],
		state: familyState(states, familySurfaces.length + familyCases.length),
		states,
		surfaces: familySurfaces,
		cases: familyCases,
	};
}

function collectContractSurfaces(input = {}) {
	const rawSurfaces = [
		...(Array.isArray(input.surfaces) ? input.surfaces : []),
		...(Array.isArray(input.unsupported_surfaces) ? input.unsupported_surfaces : []),
		...(Array.isArray(input.unsupportedSurfaces) ? input.unsupportedSurfaces : []),
	];
	return rawSurfaces.map(normalizeContractSurface).filter(Boolean);
}

function collectContractCases(input = {}) {
	const targets = Array.isArray(input.targets) ? input.targets : [];
	return targets.flatMap((target) => (Array.isArray(target.cases) ? target.cases : []).map((testCase) => normalizeContractCase(target, testCase)).filter(Boolean));
}

function normalizeContractSurface(surface = {}) {
	const type = contractType(surface.canonical_type || surface.canonicalType || surface.type || surface.kind);
	if (!type || !FAMILY_BY_TYPE.has(type)) {
		return null;
	}
	const state = normalizeExecutableState(surface.executable_state || surface.executableState || surface.execution_state || surface.executionState || surface.execution_tier || surface.executionTier, surface);
	return {
		id: stringValue(surface.id, `${type}:surface`),
		type,
		family: FAMILY_BY_TYPE.get(type).id,
		state,
		executable: state === 'read_only_executable' || state === 'isolated_mutating_executable',
		metadata: isObject(surface.metadata) ? { ...surface.metadata } : {},
	};
}

function normalizeContractCase(target = {}, testCase = {}) {
	const type = contractType(testCase.surface_type || testCase.surfaceType || target.type || target.surface_type || target.surfaceType);
	if (!type || !FAMILY_BY_TYPE.has(type)) {
		return null;
	}
	const state = normalizeExecutableState(testCase.executable_state || testCase.executableState || testCase.execution_state || testCase.executionState || testCase.execution_tier || testCase.executionTier || testCase.metadata?.execution_tier, testCase);
	return {
		id: stringValue(testCase.id, `${target.id || type}:case`),
		surface_id: stringValue(testCase.surface_id || testCase.surfaceId || target.surface_id || target.surfaceId || target.id, `${type}:surface`),
		type,
		family: FAMILY_BY_TYPE.get(type).id,
		intent: testCase.intent || null,
		state,
		executable: state === 'read_only_executable' || state === 'isolated_mutating_executable',
		reason_codes: reasonCodes(testCase),
		metadata: isObject(testCase.metadata) ? { ...testCase.metadata } : {},
	};
}

function normalizeExecutableState(value, subject = {}) {
	const normalized = String(value || '').trim().toLowerCase().replace(/-/g, '_');
	if (WORDPRESS_SURFACE_EXECUTABLE_STATES.includes(normalized)) {
		return normalized;
	}
	if (normalized === 'plan_only' || subject.executable === false || subject.metadata?.gated === true || subject.metadata?.planned === true) {
		return 'unsupported';
	}
	if (subject.metadata?.safety?.mutates === true || subject.operation?.safety?.mutates === true || reasonCodes(subject).some((reason) => reason.includes('mutation') || reason.includes('mutat'))) {
		return 'isolated_mutating_executable';
	}
	return subject.executable === true ? 'read_only_executable' : 'discovered';
}

function familyState(states, count) {
	if (states.read_only_executable > 0 || states.isolated_mutating_executable > 0) {
		return states.unsupported > 0 || states.discovered > 0 ? 'discovered' : 'read_only_executable';
	}
	if (states.discovered > 0) {
		return 'discovered';
	}
	if (states.unsupported > 0) {
		return 'unsupported';
	}
	return count > 0 ? 'discovered' : 'unsupported';
}

function countStates(items) {
	const counts = Object.fromEntries(WORDPRESS_SURFACE_EXECUTABLE_STATES.map((state) => [state, 0]));
	for (const item of items) {
		const state = WORDPRESS_SURFACE_EXECUTABLE_STATES.includes(item.state) ? item.state : 'discovered';
		counts[state] += 1;
	}
	return counts;
}

function contractType(value) {
	return String(value || '').trim().toLowerCase().replace(/_/g, '-');
}

function reasonCodes(subject = {}) {
	return [...new Set([
		...arrayOf(subject.skip_reasons || subject.skipReasons || subject.skip_reason || subject.skipReason),
		...arrayOf(subject.destructive_reasons || subject.destructiveReasons || subject.destructive_reason || subject.destructiveReason),
	].map(String).filter(Boolean))].sort();
}

function arrayOf(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return Array.isArray(value) ? value : [value];
}

function stringValue(value, fallback) {
	return String(value || fallback || '').trim();
}

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
	WORDPRESS_SURFACE_EXECUTABLE_STATES,
	WORDPRESS_SURFACE_FAMILIES,
	WORDPRESS_SURFACE_FAMILY_CONTRACT_SCHEMA,
	normalizeWordPressSurfaceFamilyContracts,
};
