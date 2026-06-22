'use strict';

/**
 * Internal dependencies
 */
const {
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	normalizeWordPressFuzzPlan,
	normalizeWordPressSurfaceDiscovery,
} = require('./wordpress-fuzz-schemas');
const {
	WORDPRESS_CRUD_OPERATION_SCHEMA,
	normalizeWordPressCrudOperation,
} = require('./wordpress-generic-fuzz-primitives');

const SURFACE_COLLECTION_KEYS = [
	'surfaces',
	'hooks',
	'cron',
	'cron_events',
	'cronEvents',
	'capabilities',
	'database',
	'db',
	'databaseTables',
	'database_tables',
	'dbQueries',
	'db_queries',
	'options',
	'post_types',
	'postTypes',
	'taxonomies',
	'media',
	'users',
	'roles',
	'blocks',
	'frontend',
	'frontendUrls',
	'frontend_urls',
	'admin',
	'adminPages',
	'admin_pages',
	'ajax',
	'actions',
	'ajaxActions',
	'ajax_actions',
	'externalHttp',
	'external_http',
	'http',
	'httpRequests',
	'http_requests',
	'rest',
	'restRoutes',
	'routes',
];
const SAFE_REST_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);

function buildWordPressFuzzPlanFromSurfaces(input = {}, options = {}) {
	const discovery = normalizeWordPressSurfaceDiscovery({
		schema: WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
		id: input.id || input.discovery_id || input.discoveryId || options.discoveryId || 'wordpress-surface-discovery',
		label: input.label || options.label || 'WordPress surface discovery',
		surfaces: collectWordPressFuzzPlanSurfaces(input),
	});

	return normalizeWordPressFuzzPlan({
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: options.id || input.plan_id || input.planId || `${discovery.id}-fuzz-plan`,
		discovery_id: discovery.id,
		targets: discovery.surfaces.map((surface) => targetFromSurface(surface, options)),
		budget: options.budget || input.budget || {},
		metadata: {
			...(input.metadata || {}),
			planner: 'homeboy/wordpress-fuzz-plan-from-surfaces/v1',
		},
	});
}

function collectWordPressFuzzPlanSurfaces(input = {}) {
	if (Array.isArray(input)) {
		return input.flatMap((surface) => collectWordPressFuzzPlanSurfaces(surface));
	}
	if (!isObject(input)) {
		return [];
	}
	if (Array.isArray(input.surfaces)) {
		return input.surfaces;
	}

	const surfaces = [];
	for (const key of SURFACE_COLLECTION_KEYS) {
		if (key === 'surfaces') {
			appendSurfaceMap(surfaces, input.surfaces, undefined);
			continue;
		}
		appendSurfaceMap(surfaces, input[key], surfaceTypeFromCollectionKey(key));
	}
	return surfaces;
}

function appendSurfaceMap(surfaces, value, defaultType) {
	if (Array.isArray(value)) {
		for (const item of value) {
			surfaces.push(surfaceFromValue(item, defaultType));
		}
		return;
	}
	if (!isObject(value)) {
		return;
	}
	if (defaultType === 'hook') {
		appendSurfaceMap(surfaces, value.actions, 'hook');
		appendSurfaceMap(surfaces, value.filters, 'hook');
	}
	if (defaultType === 'rest-route') {
		appendSurfaceMap(surfaces, value.routes, 'rest-route');
	}
	if (defaultType === 'database-table') {
		appendSurfaceMap(surfaces, value.tables, 'database-table');
		appendSurfaceMap(surfaces, value.queries, 'db-query');
	}
	if (defaultType === 'external-http') {
		appendSurfaceMap(surfaces, value.requests, 'external-http');
	}
	for (const [key, item] of Object.entries(value)) {
		if (
			(defaultType === 'hook' && ['actions', 'filters'].includes(key))
			|| (defaultType === 'rest-route' && key === 'routes')
			|| (defaultType === 'database-table' && ['tables', 'queries'].includes(key))
			|| (defaultType === 'external-http' && key === 'requests')
		) {
			continue;
		}
		surfaces.push(surfaceFromValue(isObject(item) ? { id: key, name: key, ...item } : { id: key, name: key, value: item }, defaultType));
	}
}

function surfaceFromValue(value, defaultType) {
	if (typeof value === 'string') {
		return { id: value, name: value, type: defaultType };
	}
	return { ...value, type: value.type || value.kind || defaultType };
}

function surfaceTypeFromCollectionKey(key) {
	return {
		hooks: 'hook',
		cron: 'cron-event',
		cron_events: 'cron-event',
		cronEvents: 'cron-event',
		capabilities: 'capability',
		database: 'database-table',
		db: 'database-table',
		databaseTables: 'database-table',
		database_tables: 'database-table',
		dbQueries: 'db-query',
		db_queries: 'db-query',
		options: 'option',
		post_types: 'post-type',
		postTypes: 'post-type',
		taxonomies: 'taxonomy',
		media: 'media',
		users: 'user',
		roles: 'role',
		blocks: 'block',
		frontend: 'frontend-url',
		frontendUrls: 'frontend-url',
		frontend_urls: 'frontend-url',
		admin: 'admin-page',
		adminPages: 'admin-page',
		admin_pages: 'admin-page',
		ajax: 'ajax-action',
		actions: 'ajax-action',
		ajaxActions: 'ajax-action',
		ajax_actions: 'ajax-action',
		externalHttp: 'external-http',
		external_http: 'external-http',
		http: 'external-http',
		httpRequests: 'external-http',
		http_requests: 'external-http',
		rest: 'rest-route',
		restRoutes: 'rest-route',
		routes: 'rest-route',
	}[key];
}

function targetFromSurface(surface, options = {}) {
	if (surface.type === 'admin-page') {
		return adminPageTargetFromSurface(surface, options);
	}
	if (surface.type === 'rest-route' && !crudResourceForSurface(surface)) {
		return restRouteTargetFromSurface(surface, options);
	}
	return genericTargetFromSurface(surface, options);
}

function restRouteTargetFromSurface(surface, options = {}) {
	const methods = restMethodsForSurface(surface);
	if (methods.length === 0) {
		return genericTargetFromSurface(surface, options);
	}

	const operationId = surface.operation_id || surface.operationId || `${surface.id}:request-rest-route`;
	const surfaceSkipReasons = reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason);
	const surfaceDestructiveReasons = reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons);
	return {
		id: surface.id,
		surface_id: surface.id,
		type: surface.type,
		operation_id: operationId,
		cases: methods.map((method) => restRouteCaseFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options)),
		metadata: {
			label: surface.label,
			type: surface.type,
			skip_reasons: surfaceSkipReasons,
			destructive_reasons: surfaceDestructiveReasons,
			methods,
			...(surface.metadata || {}),
		},
	};
}

function genericTargetFromSurface(surface, options = {}) {
	const cases = casesForSurface(surface, options);
	const operationId = cases[0]?.operation_id || surface.operation_id || surface.operationId || `${surface.id}:${caseIntent(surface.type)}`;
	return {
		id: surface.id,
		surface_id: surface.id,
		type: surface.type,
		operation_id: operationId,
		cases,
		metadata: {
			label: surface.label,
			type: surface.type,
			skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
			destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
			...(surface.metadata || {}),
		},
	};
}

function adminPageTargetFromSurface(surface, options = {}) {
	const operation = operationForSurface(surface);
	const operationId = surface.operation_id || surface.operationId || `${surface.id}:request-admin-page`;
	const skipReasons = reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason);
	const destructiveReasons = reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons);
	const cases = [{
		id: `${surface.id}-generic-fuzz`,
		intent: 'request-admin-page',
		operation_id: operationId,
		operation,
		seed: options.seed,
		skip_reasons: skipReasons,
		destructive_reasons: destructiveReasons,
		metadata: { surface, executable: skipReasons.length === 0 && destructiveReasons.length === 0 },
	}];

	for (const interaction of collectAdminPageInteractions(surface)) {
		cases.push(adminPageInteractionCase(surface, interaction, options));
	}

	return {
		id: surface.id,
		surface_id: surface.id,
		type: surface.type,
		operation_id: operationId,
		cases,
		metadata: {
			label: surface.label,
			type: surface.type,
			skip_reasons: skipReasons,
			destructive_reasons: destructiveReasons,
			...(surface.metadata || {}),
		},
	};
}

function restRouteCaseFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options = {}) {
	const safeMethod = SAFE_REST_METHODS.has(method);
	const operation = { ...operationForSurface(surface), method };
	const skipReasons = safeMethod ? surfaceSkipReasons : reasonList([...surfaceSkipReasons, 'mutating_rest_method_requires_explicit_opt_in']);
	const destructiveReasons = safeMethod ? surfaceDestructiveReasons : reasonList([...surfaceDestructiveReasons, 'rest_method_mutates_state']);
	return {
		id: `${surface.id}-${method.toLowerCase()}-generic-fuzz`,
		intent: 'request-rest-route',
		operation_id: operationId,
		operation,
		seed: options.seed,
		skip_reasons: skipReasons,
		destructive_reasons: destructiveReasons,
		metadata: {
			surface,
			rest_method: method,
			auth: surface.auth || surface.authentication || surface.authorization || null,
			safety: safeMethod ? { level: 'safe', mutates: false } : { level: 'mutating', mutates: true, requires_explicit_opt_in: true },
			planned: !safeMethod,
			gated: !safeMethod,
		},
	};
}

function restMethodsForSurface(surface) {
	const rawMethods = surface.methods === undefined ? surface.method : surface.methods;
	return [...new Set((Array.isArray(rawMethods) ? rawMethods : String(rawMethods || '').split(','))
		.map((method) => String(method).trim().toUpperCase())
		.filter(Boolean))].sort();
}

function casesForSurface(surface, options = {}) {
	const crudResource = crudResourceForSurface(surface);
	if (!crudResource) {
		return [genericCaseForSurface(surface, options)];
	}

	const actions = crudActionsForSurface(surface, crudResource);
	return actions.map((action) => crudCaseForSurface(surface, crudResource, action, options));
}

function genericCaseForSurface(surface, options = {}) {
	const operation = operationForSurface(surface);
	const operationId = surface.operation_id || surface.operationId || `${surface.id}:${caseIntent(surface.type)}`;
	return {
		id: `${surface.id}-generic-fuzz`,
		intent: caseIntent(surface.type),
		operation_id: operationId,
		operation,
		seed: options.seed,
		skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
		destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
		metadata: { surface },
	};
}

function crudCaseForSurface(surface, resource, action, options = {}) {
	const operation = crudOperationForSurface(surface, resource, action);
	const gateReasons = mutatingCrudAction(action.action) && !allowsCrudMutation(surface, action.action) ? ['crud_mutation_requires_explicit_allow'] : [];
	return {
		id: `${surface.id}-${action.intent}-crud-fuzz`,
		intent: action.intent,
		operation_id: operation.id,
		operation,
		seed: options.seed,
		skip_reasons: [
			...reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
			...gateReasons,
		],
		destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
		metadata: { surface, crud: { resource_type: resource.type, intent: action.intent, action: action.action } },
	};
}

function crudOperationForSurface(surface, resource, action) {
	return normalizeWordPressCrudOperation({
		schema: WORDPRESS_CRUD_OPERATION_SCHEMA,
		id: `${surface.id}:${action.intent}`,
		action: action.action,
		resource_type: resource.type,
		label: `${action.intent} ${resource.type}`,
		capability_context: capabilityContextForCrudAction(surface, resource, action.action),
		transport: transportForCrudAction(surface, resource, action),
		input: inputForCrudAction(surface, resource, action.action),
		expected: { intent: action.intent },
		rollback_policy: rollbackPolicyForCrudAction(action.action),
		metadata: {
			...resource.metadata,
			intent: action.intent,
			surface_id: surface.id,
			surface_type: surface.type,
		},
	});
}

function crudResourceForSurface(surface) {
	const explicitType = surface.resource_type || surface.resourceType || surface.resource || surface.object_type || surface.objectType;
	if (explicitType) {
		return { type: String(explicitType), metadata: resourceMetadataForSurface(surface) };
	}
	if (surface.type === 'post-type') {
		return { type: 'post', metadata: { post_type: surface.post_type || surface.postType || surface.name || surface.id } };
	}
	if (surface.type === 'taxonomy') {
		return { type: 'term', metadata: { taxonomy: surface.taxonomy || surface.name || surface.id } };
	}
	if (surface.type === 'user') {
		return { type: 'user', metadata: resourceMetadataForSurface(surface) };
	}
	if (surface.type === 'option') {
		return { type: 'option', metadata: { option: surface.option || surface.name || surface.id } };
	}
	return null;
}

function resourceMetadataForSurface(surface) {
	const metadata = {};
	for (const key of ['post_type', 'postType', 'taxonomy', 'option', 'setting', 'name', 'route', 'method']) {
		if (surface[key] !== undefined) {
			metadata[key] = surface[key];
		}
	}
	return metadata;
}

function crudActionsForSurface(surface, resource) {
	const safeActions = resource.type === 'option'
		? [{ action: 'read', intent: 'read-option' }]
		: [
			{ action: 'read', intent: `list-${resource.type}s` },
			{ action: 'read', intent: `read-${resource.type}` },
		];
	const mutating = ['create', 'update', 'delete'].map((action) => ({ action, intent: `${action}-${resource.type}` }));
	return [...safeActions, ...mutating];
}

function allowsCrudMutation(surface, action) {
	if (surface.allow_mutations === true || surface.allowMutations === true || surface.allow_crud_mutations === true || surface.allowCrudMutations === true) {
		return true;
	}
	return reasonList(surface.crud_actions || surface.crudActions || surface.actions || []).includes(action);
}

function mutatingCrudAction(action) {
	return ['create', 'update', 'delete'].includes(action);
}

function capabilityContextForCrudAction(surface, resource, action) {
	const explicit = surface.capability_context || surface.capabilityContext;
	if (explicit) {
		return explicit;
	}
	const capability = surface.capability || defaultCrudCapability(resource.type, action);
	return capability ? { required: [capability] } : undefined;
}

function defaultCrudCapability(resourceType, action) {
	if (resourceType === 'option' || resourceType === 'setting') {
		return 'manage_options';
	}
	if (resourceType === 'user') {
		return { create: 'create_users', read: 'list_users', update: 'edit_users', delete: 'delete_users' }[action];
	}
	if (resourceType === 'post') {
		return { create: 'edit_posts', read: 'read', update: 'edit_posts', delete: 'delete_posts' }[action];
	}
	if (resourceType === 'term' && mutatingCrudAction(action)) {
		return 'manage_categories';
	}
	return undefined;
}

function transportForCrudAction(surface, resource, action) {
	if (surface.transport) {
		return surface.transport;
	}
	return stripUndefined({
		type: surface.route ? 'rest' : undefined,
		method: action.action === 'read' ? (surface.method || 'GET') : undefined,
		route: surface.route,
		path: surface.path,
	});
}

function inputForCrudAction(surface, resource, action) {
	const input = {
		...resource.metadata,
		...(surface.input || {}),
	};
	if (action === 'create') {
		Object.assign(input, surface.create_input || surface.createInput || {});
	}
	if (action === 'update') {
		Object.assign(input, surface.update_input || surface.updateInput || {});
	}
	if (action === 'delete') {
		Object.assign(input, surface.delete_input || surface.deleteInput || {});
	}
	return Object.keys(input).length > 0 ? input : undefined;
}

function rollbackPolicyForCrudAction(action) {
	if (action === 'read') {
		return { strategy: 'none', scope: 'operation', after_each_case: false };
	}
	if (action === 'create') {
		return { strategy: 'delete-created', scope: 'operation', after_each_case: true };
	}
	return { strategy: 'restore-snapshot', scope: 'operation', after_each_case: true };
}

function collectAdminPageInteractions(surface) {
	return [
		...tagAdminPageInteractions(surface.interactions, 'interaction'),
		...tagAdminPageInteractions(surface.forms, 'form'),
		...tagAdminPageInteractions(surface.actions, 'action'),
	];
}

function tagAdminPageInteractions(items, kind) {
	return Array.isArray(items) ? items.map((item, index) => ({ kind, index, ...(isObject(item) ? item : { value: item }) })) : [];
}

function adminPageInteractionCase(surface, interaction, options = {}) {
	const operation = stripUndefined({
		...operationForSurface(surface),
		interaction_kind: interaction.kind,
		interaction_id: interaction.id || interaction.name || interaction.selector || interaction.action || `${interaction.kind}-${interaction.index + 1}`,
		selector: interaction.selector,
		action: interaction.action,
		method: interaction.method,
		fields: interaction.fields,
	});
	const safety = adminPageInteractionSafety(interaction);
	const skipReasons = reasonList(interaction.skip_reasons || interaction.skipReasons || interaction.skip_reason || interaction.skipReason);
	const destructiveReasons = reasonList(interaction.destructive_reasons || interaction.destructiveReasons || interaction.destructive_reason || interaction.destructiveReason || safety.reason_codes);
	const gated = safety.mutates || destructiveReasons.length > 0;
	if (gated) {
		skipReasons.push('requires_explicit_mutation_opt_in');
	}

	return {
		id: `${surface.id}-${interaction.kind}-${normalizeToken(operation.interaction_id)}-plan`,
		intent: gated ? 'plan-admin-page-mutation' : 'exercise-admin-page-read-only-interaction',
		operation_id: `${surface.id}:${interaction.kind}:${normalizeToken(operation.interaction_id)}`,
		operation,
		seed: options.seed,
		skip_reasons: [...new Set(skipReasons)].sort(),
		destructive_reasons: destructiveReasons,
		metadata: stripUndefined({
			interaction,
			safety,
			capability_context: normalizeCapabilityContext(interaction),
			nonce_context: normalizeNonceContext(interaction),
			executable: !gated,
			gated,
			requires_explicit_opt_in: gated || undefined,
		}),
	};
}

function adminPageInteractionSafety(interaction) {
	const declared = isObject(interaction.safety) ? interaction.safety : {};
	const method = String(interaction.method || declared.method || 'GET').toUpperCase();
	const mutates = interaction.mutates === true || interaction.destructive === true || declared.mutates === true || !['GET', 'HEAD'].includes(method);
	const destructive = interaction.destructive === true || declared.level === 'destructive';
	let level = declared.level;
	if (!level) {
		level = 'safe';
		if (destructive) {
			level = 'destructive';
		} else if (mutates) {
			level = 'mutating';
		}
	}
	return {
		level,
		mutates,
		rollback_required: declared.rollback_required ?? declared.rollbackRequired ?? mutates,
		reason_codes: reasonList(declared.reason_codes || declared.reasonCodes || (mutates ? [`${interaction.kind}_mutation`] : [])),
	};
}

function normalizeCapabilityContext(interaction) {
	const context = interaction.capability_context || interaction.capabilityContext;
	if (isObject(context)) {
		return context;
	}
	if (interaction.capability) {
		return { required: [String(interaction.capability)] };
	}
	return undefined;
}

function normalizeNonceContext(interaction) {
	const context = interaction.nonce_context || interaction.nonceContext;
	if (isObject(context)) {
		return context;
	}
	if (interaction.nonce || interaction.nonce_action || interaction.nonceAction) {
		return stripUndefined({
			required: true,
			action: interaction.nonce_action || interaction.nonceAction || interaction.action,
			field: interaction.nonce_field || interaction.nonceField || '_wpnonce',
		});
	}
	return undefined;
}

function operationForSurface(surface) {
	const operation = { id: surface.operation_id || surface.operationId, surface_type: surface.type };
	for (const key of ['id', 'name', 'hook', 'action', 'event', 'option', 'post_type', 'taxonomy', 'block_name', 'path', 'route', 'method', 'url', 'role', 'capability', 'table', 'query', 'request', 'endpoint']) {
		if (surface[key] !== undefined) {
			operation[key] = surface[key];
		}
	}
	return stripUndefined(operation);
}

function caseIntent(type) {
	return {
		'admin-page': 'request-admin-page',
		'ajax-action': 'exercise-ajax-action',
		block: 'render-block',
		'cron-event': 'inspect-cron-event',
		capability: 'check-capability-boundary',
		'database-table': 'inspect-database-table',
		'db-query': 'profile-database-query',
		'external-http': 'exercise-external-http-guardrail',
		'frontend-url': 'request-frontend-url',
		hook: 'exercise-hook',
		media: 'query-media',
		option: 'read-option',
		'post-type': 'query-post-type',
		'rest-route': 'request-rest-route',
		role: 'check-role-boundary',
		taxonomy: 'query-taxonomy',
		user: 'query-user',
	}[type] || 'exercise-wordpress-surface';
}

function reasonList(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set((Array.isArray(value) ? value : [value]).map(String).filter(Boolean))].sort();
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

function normalizeToken(value) {
	return String(value || '')
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '_')
		.replace(/^_+|_+$/g, '') || 'interaction';
}

module.exports = {
	buildWordPressFuzzPlanFromSurfaces,
	collectWordPressFuzzPlanSurfaces,
};
