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
const {
	WORDPRESS_SURFACE_COLLECTION_KEYS,
	wordpressSurfaceTypeFromCollectionKey,
} = require('./wordpress-surface-types');
const {
	annotateWordPressFuzzCaseExecutionTier,
	gateWordPressFuzzCaseForRuntimeCapabilities,
	normalizeWordPressFuzzMutationMode,
	requiredCapabilitiesForWordPressFuzzCase,
} = require('./wordpress-fuzz-runtime-capabilities');
const {
	buildWordPressFuzzMutationLifecycleContract,
} = require('./wordpress-fuzz-mutation-lifecycle');
const {
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
} = require('./wordpress-fuzz-runtime-workload-operations');

const SAFE_REST_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);
const DB_MUTATION_REQUIRED_CAPABILITIES = requiredCapabilitiesForWordPressFuzzCase('db_mutation');
const REST_ROLLBACK_ANY_CAPABILITIES = Object.freeze([Object.freeze(['restore', 'reset'])]);

function buildWordPressFuzzPlanFromSurfaces(input = {}, options = {}) {
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode || input.mutation_mode || input.mutationMode);
	const targetOptions = mutationMode ? { ...options, mutation_mode: mutationMode } : options;
	const discovery = normalizeWordPressSurfaceDiscovery({
		schema: WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
		id: input.id || input.discovery_id || input.discoveryId || options.discoveryId || 'wordpress-surface-discovery',
		label: input.label || options.label || 'WordPress surface discovery',
		surfaces: collectWordPressFuzzPlanSurfaces(input),
	});
	const targets = decoratePlanTargets(discovery.surfaces.map((surface) => targetFromSurface(surface, targetOptions)), targetOptions);

	return normalizeWordPressFuzzPlan({
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: options.id || input.plan_id || input.planId || `${discovery.id}-fuzz-plan`,
		discovery_id: discovery.id,
		targets,
		budget: options.budget || input.budget || {},
		metadata: {
			...(input.metadata || {}),
			planner: 'homeboy/wordpress-fuzz-plan-from-surfaces/v1',
			mutation_mode: mutationMode || undefined,
			execution_tiers: summarizeExecutionTiers(targets),
		},
	});
}

function decoratePlanTargets(targets, options = {}) {
	return targets.map((target) => {
		const cases = target.cases.map((testCase) => attachWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase, options));
		const metadata = {
			...(target.metadata || {}),
			execution_tiers: summarizeExecutionTiers([{ ...target, cases }]),
		};
		return { ...target, cases, metadata };
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
	for (const key of WORDPRESS_SURFACE_COLLECTION_KEYS) {
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
	return wordpressSurfaceTypeFromCollectionKey(key);
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
	const target = {
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
	target.metadata.execution_tiers = summarizeExecutionTiers([target]);
	return target;
}

function genericTargetFromSurface(surface, options = {}) {
	const cases = casesForSurface(surface, options);
	const operationId = cases[0]?.operation_id || surface.operation_id || surface.operationId || `${surface.id}:${caseIntent(surface.type)}`;
	const target = {
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
	target.metadata.execution_tiers = summarizeExecutionTiers([target]);
	return target;
}

function adminPageTargetFromSurface(surface, options = {}) {
	const operation = operationForSurface(surface);
	const operationId = surface.operation_id || surface.operationId || `${surface.id}:request-admin-page`;
	const skipReasons = reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason);
	const destructiveReasons = reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons);
	const cases = [annotateWordPressFuzzCaseExecutionTier({
		id: `${surface.id}-generic-fuzz`,
		intent: 'request-admin-page',
		operation_id: operationId,
		operation,
		seed: options.seed,
		skip_reasons: skipReasons,
		destructive_reasons: destructiveReasons,
		metadata: { surface, executable: skipReasons.length === 0 && destructiveReasons.length === 0 },
	}, { executable: skipReasons.length === 0 && destructiveReasons.length === 0 })];

	for (const interaction of collectAdminPageInteractions(surface)) {
		cases.push(adminPageInteractionCase(surface, interaction, options));
	}

	const target = {
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
	target.metadata.execution_tiers = summarizeExecutionTiers([target]);
	return target;
}

function restRouteCaseFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options = {}) {
	const safeMethod = SAFE_REST_METHODS.has(method);
	const fixtureBinding = fixtureBindingForSurfaceAction(surface, method.toLowerCase(), options);
	const operation = bindRestOperationFixtures({ ...operationForSurface(surface), method }, fixtureBinding);
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	const skipReasons = safeMethod || mutationMode === 'isolated' ? surfaceSkipReasons : reasonList([...surfaceSkipReasons, 'mutating_rest_method_requires_explicit_opt_in']);
	const destructiveReasons = safeMethod ? surfaceDestructiveReasons : reasonList([...surfaceDestructiveReasons, 'rest_method_mutates_state']);
	const fixtureMetadata = fixtureBindingMetadata(fixtureBinding);
	const testCase = annotateWordPressFuzzCaseExecutionTier({
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
			fixture_binding: fixtureMetadata,
			auth: surface.auth || surface.authentication || surface.authorization || null,
			safety: safeMethod ? { level: 'safe', mutates: false } : { level: 'mutating', mutates: true, requires_explicit_opt_in: true, rollback_required: true },
			mutation_lifecycle: safeMethod ? undefined : mutationLifecycleContract({ kind: 'rest', surface, method }),
			rollback_contract: safeMethod ? undefined : restMutationRollbackContract({ surface, method }),
			planned: !safeMethod,
			gated: !safeMethod,
		},
	}, { mutates: !safeMethod, executable: skipReasons.length === 0 && (safeMethod || mutationMode === 'isolated') });
	if (safeMethod) {
		return testCase;
	}
	return gateWordPressFuzzCaseForRuntimeCapabilities(testCase, options.runtimeCapabilities || options.runtime_capabilities, {
		required_capabilities: requiredCapabilitiesForWordPressFuzzCase('mutating_rest'),
		required_any_capabilities: REST_ROLLBACK_ANY_CAPABILITIES,
		mutation_mode: mutationMode,
		mutates: true,
	});
}

function restMethodsForSurface(surface) {
	const rawMethods = surface.methods === undefined ? surface.method : surface.methods;
	return [...new Set((Array.isArray(rawMethods) ? rawMethods : String(rawMethods || '').split(','))
		.map((method) => String(method).trim().toUpperCase())
		.filter(Boolean))].sort();
}

function casesForSurface(surface, options = {}) {
	if (surface.type === 'block') {
		const operation = operationForSurface(surface);
		const operationId = surface.operation_id || surface.operationId || `${surface.id}:${caseIntent(surface.type)}`;
		return blockCasesFromSurface(surface, operation, operationId, options);
	}
	if (['database-table', 'db-query'].includes(surface.type)) {
		const testCase = genericCaseForSurface(surface, options);
		return [testCase, ...dbMutationCasesFromSurface(surface, testCase.operation_id, options)];
	}
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
	return annotateWordPressFuzzCaseExecutionTier({
		id: `${surface.id}-generic-fuzz`,
		intent: caseIntent(surface.type),
		operation_id: operationId,
		operation,
		seed: options.seed,
		skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
		destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
		metadata: { surface },
	}, { mutates: false });
}

function crudCaseForSurface(surface, resource, action, options = {}) {
	const fixtureBinding = fixtureBindingForSurfaceAction(surface, action.action, options);
	const operation = bindCrudOperationFixtures(crudOperationForSurface(surface, resource, action), fixtureBinding);
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	const mutates = mutatingCrudAction(action.action);
	const restTransport = operation.transport?.type === 'rest';
	const requiredCapabilities = mutates && restTransport
		? requiredCapabilitiesForWordPressFuzzCase('rest_crud_mutation')
		: requiredCapabilitiesForWordPressFuzzCase('mutating_crud');
	const gateReasons = crudMutationGateReasons(surface, action.action, mutates, mutationMode);
	const fixtureMetadata = fixtureBindingMetadata(fixtureBinding);
	const testCase = {
		id: `${surface.id}-${action.intent}-crud-fuzz`,
		intent: action.intent,
		operation_id: operation.id,
		operation,
		seed: options.seed,
		required_capabilities: mutates ? requiredCapabilities : undefined,
		skip_reasons: [
			...reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
			...gateReasons,
		],
		destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
		metadata: stripUndefined({
			surface,
			crud: { resource_type: resource.type, intent: action.intent, action: action.action },
			fixture_binding: fixtureMetadata,
			mutation_lifecycle: mutates ? mutationLifecycleContract({ kind: restTransport ? 'rest_crud' : 'crud', surface, method: operation.transport?.method, action: action.action }) : undefined,
			rollback_contract: mutates && restTransport ? restMutationRollbackContract({ surface, method: operation.transport.method, action: action.action }) : undefined,
		}),
	};
	if (!mutates || gateReasons.length > 0) {
		return annotateWordPressFuzzCaseExecutionTier(testCase, { mutates, executable: !mutates && testCase.skip_reasons.length === 0 });
	}
	return gateWordPressFuzzCaseForRuntimeCapabilities(testCase, options.runtimeCapabilities || options.runtime_capabilities, {
		required_capabilities: requiredCapabilities,
		required_any_capabilities: restTransport ? REST_ROLLBACK_ANY_CAPABILITIES : undefined,
		mutation_mode: mutationMode,
		mutates: true,
	});
}

function restMutationRollbackContract({ surface = {}, method, action } = {}) {
	return stripUndefined({
		schema: 'homeboy/wordpress-rest-mutation-rollback-contract/v1',
		strategy: 'checkpoint-restore-or-reset',
		required_capabilities: requiredCapabilitiesForWordPressFuzzCase(action ? 'rest_crud_mutation' : 'mutating_rest'),
		required_any_capabilities: REST_ROLLBACK_ANY_CAPABILITIES.map((group) => [...group]),
		restore_boundary: 'after_each_case',
		delete_boundary_artifacts: method === 'DELETE' || action === 'delete',
		route: surface.route,
		method,
		action,
	});
}

function mutationLifecycleContract({ kind, surface = {}, method, action } = {}) {
	return buildWordPressFuzzMutationLifecycleContract({
		kind,
		method,
		action,
		delete_boundary: method === 'DELETE' || action === 'delete',
		metadata: stripUndefined({
			surface_id: surface.id,
			surface_type: surface.type,
			route: surface.route,
			path: surface.path,
			table: surface.table,
		}),
	});
}

function crudMutationGateReasons(surface, action, mutates, mutationMode) {
	if (!mutates || mutationMode) {
		return [];
	}
	if (!allowsCrudMutation(surface, action)) {
		return ['crud_mutation_requires_explicit_allow'];
	}
	return ['requires-isolated-mutation-runtime'];
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
		method: surface.route ? restMethodForCrudAction(surface, action.action) : undefined,
		route: surface.route,
		path: surface.path,
	});
}

function restMethodForCrudAction(surface, action) {
	if (surface.method && action === 'read') {
		return surface.method;
	}
	return { create: 'POST', read: 'GET', update: 'PUT', delete: 'DELETE' }[action];
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

function fixtureBindingForSurfaceAction(surface, action, options = {}) {
	const plannerBinding = fixtureBindingFromPlannerOptions(surface, options);
	const surfaceBinding = surface.fixture_bindings || surface.fixtureBindings || surface.fixture_binding || surface.fixtureBinding || surface.fixtures;
	const binding = mergeFixtureBindings(normalizeFixtureBinding(plannerBinding, action), normalizeFixtureBinding(surfaceBinding, action));
	return bindingHasValues(binding) ? binding : undefined;
}

function fixtureBindingFromPlannerOptions(surface, options = {}) {
	const bindings = options.fixture_bindings || options.fixtureBindings || options.rest_fixture_bindings || options.restFixtureBindings;
	if (!isObject(bindings)) {
		return undefined;
	}
	return bindings[surface.id]
		|| bindings[surface.route]
		|| bindings[surface.path]
		|| bindings[surface.name]
		|| bindings[surface.resource_type]
		|| bindings[surface.resourceType]
		|| bindings.default;
}

function normalizeFixtureBinding(binding, action) {
	if (!isObject(binding)) {
		return {};
	}
	const scoped = isObject(binding[action]) ? binding[action] : {};
	const routeParams = binding.route_params || binding.routeParams || binding.path_params || binding.pathParams || binding.params;
	const scopedRouteParams = scoped.route_params || scoped.routeParams || scoped.path_params || scoped.pathParams || scoped.params;
	const payloads = binding.request_bodies || binding.requestBodies || binding.payloads || binding.bodies || binding.body;
	const scopedPayloads = scoped.request_bodies || scoped.requestBodies || scoped.payloads || scoped.bodies || scoped.body;
	return stripUndefined({
		fixture_id: binding.fixture_id || binding.fixtureId || scoped.fixture_id || scoped.fixtureId,
		artifact_ref: binding.artifact_ref || binding.artifactRef || scoped.artifact_ref || scoped.artifactRef,
		route_params: normalizeFixtureBindingMap({ ...(isObject(routeParams) ? routeParams : {}), ...(isObject(scopedRouteParams) ? scopedRouteParams : {}) }),
		request_body: normalizeFixtureValue(actionScopedMapValue(payloads, action) ?? actionScopedMapValue(scopedPayloads, action)),
	});
}

function actionScopedMapValue(value, action) {
	if (value === undefined || value === null) {
		return undefined;
	}
	if (!isObject(value)) {
		return value;
	}
	const actionKeys = ['create', 'read', 'update', 'delete', 'get', 'post', 'put', 'patch'];
	if (actionKeys.some((key) => Object.prototype.hasOwnProperty.call(value, key))) {
		return value[action];
	}
	return value;
}

function mergeFixtureBindings(base, override) {
	return stripUndefined({
		fixture_id: override.fixture_id || base.fixture_id,
		artifact_ref: override.artifact_ref || base.artifact_ref,
		route_params: normalizeFixtureBindingMap({ ...(base.route_params || {}), ...(override.route_params || {}) }),
		request_body: override.request_body || base.request_body,
	});
}

function normalizeFixtureBindingMap(value) {
	if (!isObject(value)) {
		return undefined;
	}
	const entries = Object.entries(value).map(([key, entry]) => [key, normalizeFixtureValue(entry)]).filter(([, entry]) => entry !== undefined);
	return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

function normalizeFixtureValue(entry) {
	if (entry === undefined || entry === null) {
		return undefined;
	}
	if (!isObject(entry) || Array.isArray(entry)) {
		return { value: entry };
	}
	const hasReferenceShape = ['value', 'id', 'ref', 'fixture_id', 'fixtureId', 'artifact_ref', 'artifactRef', 'artifact', 'source_artifact', 'sourceArtifact', 'path', 'json_path', 'jsonPath', 'pointer', 'metadata']
		.some((key) => Object.prototype.hasOwnProperty.call(entry, key));
	if (!hasReferenceShape) {
		return { value: entry };
	}
	return stripUndefined({
		value: entry.value ?? entry.id ?? entry.ref,
		fixture_id: entry.fixture_id || entry.fixtureId,
		artifact_ref: entry.artifact_ref || entry.artifactRef || entry.artifact || entry.source_artifact || entry.sourceArtifact,
		path: entry.path || entry.json_path || entry.jsonPath || entry.pointer,
		metadata: isObject(entry.metadata) ? entry.metadata : undefined,
	});
}

function bindingHasValues(binding) {
	return Boolean(binding && (binding.fixture_id || binding.artifact_ref || binding.request_body || Object.keys(binding.route_params || {}).length > 0));
}

function bindCrudOperationFixtures(operation, binding) {
	if (!bindingHasValues(binding)) {
		return operation;
	}
	return normalizeWordPressCrudOperation({
		...operation,
		transport: bindRestTransportFixtures(operation.transport, binding),
		input: stripUndefined({
			...(operation.input || {}),
			route_params: fixtureValues(binding.route_params),
			request_body: binding.request_body?.value,
		}),
		metadata: stripUndefined({
			...(operation.metadata || {}),
			fixture_binding: fixtureBindingMetadata(binding),
		}),
	});
}

function bindRestOperationFixtures(operation, binding) {
	if (!bindingHasValues(binding)) {
		return operation;
	}
	return stripUndefined({
		...operation,
		...bindRestPathFields(operation, binding),
		route_params: fixtureValues(binding.route_params),
		request_body: binding.request_body?.value,
	});
}

function bindRestTransportFixtures(transport, binding) {
	if (!isObject(transport)) {
		return transport;
	}
	const boundFields = bindRestPathFields(transport, binding);
	const routeTemplates = stripUndefined({
		route_template: boundFields.route_template,
		path_template: boundFields.path_template,
	});
	return stripUndefined({
		...transport,
		...boundFields,
		metadata: Object.keys(routeTemplates).length > 0 ? stripUndefined({ ...(transport.metadata || {}), ...routeTemplates }) : transport.metadata,
	});
}

function bindRestPathFields(container, binding) {
	const routeParams = fixtureValues(binding.route_params);
	if (!routeParams || Object.keys(routeParams).length === 0) {
		return {};
	}
	const bound = {};
	for (const key of ['route', 'path']) {
		if (container[key] !== undefined) {
			bound[`${key}_template`] = container[key];
			bound[key] = bindRouteTemplate(container[key], routeParams);
		}
	}
	return bound;
}

function bindRouteTemplate(route, params = {}) {
	return String(route).replace(/\(\?P<([^>]+)>[^)]+\)/g, (match, name) => {
		if (params[name] === undefined || params[name] === null) {
			return match;
		}
		return encodeURIComponent(String(params[name]));
	});
}

function fixtureValues(map = {}) {
	if (!isObject(map)) {
		return undefined;
	}
	const entries = Object.entries(map).filter(([, entry]) => entry && entry.value !== undefined).map(([key, entry]) => [key, entry.value]);
	return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

function fixtureBindingMetadata(binding) {
	if (!bindingHasValues(binding)) {
		return undefined;
	}
	return stripUndefined({
		fixture_id: binding.fixture_id,
		artifact_ref: binding.artifact_ref,
		route_params: binding.route_params,
		request_body: binding.request_body,
	});
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

function blockCasesFromSurface(surface, operation, operationId, options = {}) {
	const shared = {
		seed: options.seed,
		skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
		destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
	};
	const attributeSample = blockAttributeSample(surface);
	const surfaceMetadata = blockSurfaceMetadata(surface, attributeSample);
	const cases = [
		annotateWordPressFuzzCaseExecutionTier({
			id: `${surface.id}-render-block`,
			intent: 'render-block',
			operation_id: `${operationId}:render`,
			operation: stripUndefined({ ...operation, lifecycle: 'render', attributes_sample: attributeSample }),
			...shared,
			metadata: stripUndefined({ surface, safety: { mutation: 'read_only' }, attributes_sample: attributeSample }),
		}, { mutates: false }),
	];

	if (Object.keys(surfaceMetadata).length > 0) {
		cases.push(annotateWordPressFuzzCaseExecutionTier({
			id: `${surface.id}-serialize-parse-block`,
			intent: 'serialize-parse-block',
			operation_id: `${operationId}:serialize-parse`,
			operation: stripUndefined({ ...operation, lifecycle: 'serialize-parse', attributes_sample: attributeSample }),
			...shared,
			metadata: stripUndefined({ surface, safety: { mutation: 'read_only' }, ...surfaceMetadata }),
		}, { mutates: false }));
	}

	cases.push(annotateWordPressFuzzCaseExecutionTier({
		id: `${surface.id}-editor-insert-block`,
		intent: 'insert-block-in-editor',
		operation_id: `${operationId}:editor-insert`,
		operation: stripUndefined({ ...operation, lifecycle: 'editor-insert', attributes_sample: attributeSample }),
		...shared,
		skip_reasons: reasonList([...shared.skip_reasons, 'requires_browser_editor_runtime']),
		metadata: stripUndefined({
			surface,
			planned: true,
			gated: true,
			requires_runtime: ['browser', 'block-editor'],
			safety: { mutation: 'requires_isolated_editor_draft' },
			attributes_sample: attributeSample,
		}),
	}, { mutates: true, executable: false }));

	return cases;
}

function dbMutationCasesFromSurface(surface, operationId, options = {}) {
	const mutations = normalizeMutationMetadata(surface.mutations || surface.mutation || surface.mutation_metadata || surface.mutationMetadata);
	return mutations.map((mutation, index) => {
		const mutationId = mutation.id || mutation.name || mutation.operation || `mutation-${index + 1}`;
		return gateWordPressFuzzCaseForRuntimeCapabilities({
			id: `${surface.id}-${safeIdPart(mutationId)}-gated-mutation`,
			intent: surface.type === 'database-table' ? 'mutate-database-table' : 'mutate-database-query',
			operation_id: `${operationId}:${safeIdPart(mutationId)}`,
			operation: stripUndefined({
				...operationForSurface(surface),
				mutation: mutation.operation || mutation.name || mutation.type || mutationId,
				statement: mutation.statement || mutation.sql,
			}),
			seed: options.seed,
			required_capabilities: DB_MUTATION_REQUIRED_CAPABILITIES,
			skip_reasons: [],
			destructive_reasons: ['db-mutation'],
			metadata: { surface, mutation, mutation_lifecycle: mutationLifecycleContract({ kind: 'database', surface }) },
		}, options.runtimeCapabilities || options.runtime_capabilities, {
			mutation_mode: options.mutation_mode || options.mutationMode,
			mutates: true,
		});
	});
}

function normalizeMutationMetadata(value) {
	if (value === undefined || value === null || value === false) {
		return [];
	}
	if (value === true) {
		return [{ id: 'declared-mutation', operation: 'declared-mutation' }];
	}
	if (Array.isArray(value)) {
		return value.map((item, index) => mutationObject(item, index));
	}
	if (isObject(value)) {
		if (value.id || value.name || value.operation || value.type || value.statement || value.sql) {
			return [value];
		}
		return Object.entries(value).map(([key, item]) => mutationObject(isObject(item) ? { id: key, ...item } : { id: key, operation: item }, 0));
	}
	return [mutationObject(value, 0)];
}

function mutationObject(value, index) {
	if (isObject(value)) {
		return value;
	}
	return { id: `mutation-${index + 1}`, operation: String(value) };
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
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	if (gated && mutationMode !== 'isolated') {
		skipReasons.push('requires_explicit_mutation_opt_in');
	}

	const testCase = {
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
			mutation_lifecycle: gated ? mutationLifecycleContract({ kind: 'admin', surface, method: interaction.method, action: interaction.action }) : undefined,
			executable: !gated,
			gated,
			requires_explicit_opt_in: gated || undefined,
		}),
	};
	if (!gated) {
		return annotateWordPressFuzzCaseExecutionTier(testCase, { mutates: false });
	}
	return gateWordPressFuzzCaseForRuntimeCapabilities(testCase, options.runtimeCapabilities || options.runtime_capabilities, {
		required_capabilities: requiredCapabilitiesForWordPressFuzzCase('admin_mutation'),
		mutation_mode: mutationMode,
		mutates: true,
	});
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

function blockAttributeSample(surface) {
	return objectOrUndefined(surface.attributes_sample || surface.attributeSample || surface.sample_attributes || surface.sampleAttributes || surface.attributesSample || surface.example_attributes || surface.exampleAttributes);
}

function blockSurfaceMetadata(surface, attributeSample) {
	return stripUndefined({
		block_metadata: nonEmptyObjectOrUndefined(surface.block_metadata || surface.blockMetadata || surface.metadata),
		attributes_schema: nonEmptyObjectOrUndefined(surface.attributes_schema || surface.attributesSchema || surface.attributes),
		attributes_sample: attributeSample,
	});
}

function reasonList(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set((Array.isArray(value) ? value : [value]).map(String).filter(Boolean))].sort();
}

function safeIdPart(value) {
	return String(value || 'mutation')
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '') || 'mutation';
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function summarizeExecutionTiers(targets) {
	const summary = {};
	for (const target of targets) {
		for (const testCase of target.cases || []) {
			const tier = testCase.execution_tier || testCase.metadata?.execution_tier || 'discovered';
			summary[tier] = (summary[tier] || 0) + 1;
		}
	}
	return summary;
}

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

function objectOrUndefined(value) {
	return isObject(value) ? value : undefined;
}

function nonEmptyObjectOrUndefined(value) {
	return isObject(value) && Object.keys(value).length > 0 ? value : undefined;
}

function normalizeToken(value) {
	return String(value || '')
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '_')
		.replace(/^_+|_+$/g, '') || 'interaction';
}

module.exports = {
	DB_MUTATION_REQUIRED_CAPABILITIES,
	buildWordPressFuzzPlanFromSurfaces,
	collectWordPressFuzzPlanSurfaces,
};
