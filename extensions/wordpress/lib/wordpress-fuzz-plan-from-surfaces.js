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
	wordpressFuzzMutationModeAllowsIsolatedExecution,
} = require('./wordpress-fuzz-runtime-capabilities');
const {
	buildWordPressFuzzMutationLifecycleContract,
} = require('./wordpress-fuzz-mutation-lifecycle');
const {
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
} = require('./wordpress-fuzz-runtime-workload-operations');
const {
	wpCodeboxRuntimeActionTarget,
} = require('./wordpress-fuzz-runtime-action-contracts');
const {
	normalizeWordPressSurfaceFamilyContracts,
} = require('./wordpress-surface-family-contracts');

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
	const targets = decoratePlanTargets([
		...discovery.surfaces.map((surface) => targetFromSurface(surface, targetOptions)),
		...statefulSequenceTargetsFromSurfaces(discovery.surfaces, targetOptions),
	], targetOptions);
	const surfaceFamilyContracts = normalizeWordPressSurfaceFamilyContracts({
		id: `${discovery.id}-surface-family-contracts`,
		surfaces: discovery.surfaces,
		targets,
	});

	return normalizeWordPressFuzzPlan({
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: options.id || input.plan_id || input.planId || `${discovery.id}-fuzz-plan`,
		discovery_id: discovery.id,
		targets,
		budget: options.budget || input.budget || {},
		metadata: {
			...(input.metadata || {}),
			planner: 'homeboy/wordpress-fuzz-plan-from-surfaces/v1',
			surface_family_contracts: surfaceFamilyContracts,
			mutation_mode: mutationMode || undefined,
			execution_tiers: summarizeExecutionTiers(targets),
			diagnostics: wordpressFuzzPlanDiagnostics(targets),
		},
	});
}

function decoratePlanTargets(targets, options = {}) {
	return targets.map((target) => {
		const cases = target.cases.map((testCase) => attachWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase, options));
		const metadata = {
			...(target.metadata || {}),
			execution_tiers: summarizeExecutionTiers([{ ...target, cases }]),
			diagnostics: wordpressFuzzPlanDiagnostics([{ ...target, cases }]),
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
		cases: methods.flatMap((method) => restRouteCasesFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options)),
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
	if (wordpressFuzzAggressiveIsolatedMode(options) && collectAdminPageInteractions(surface).length === 0) {
		cases.push(adminPageActionDiscoveryCase(surface, options));
	}
	if (wordpressFuzzAggressiveIsolatedMode(options)) {
		cases.push(randomWalkCaseFromSurface(surface, 'admin', options));
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
	const restMutationOptIn = safeMethod ? undefined : restMutationOptInForSurfaceAction(surface, method, options);
	const fixtureBinding = fixtureBindingForSurfaceAction(surface, method.toLowerCase(), options);
	const operation = bindRestOperationFixtures({ ...operationForSurface(surface), method }, fixtureBinding);
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	const hasMutationOptIn = objectHasValues(restMutationOptIn);
	const isolatedMutationMode = wordpressFuzzMutationModeAllowsIsolatedExecution(mutationMode);
	const skipReasons = safeMethod || isolatedMutationMode || hasMutationOptIn ? surfaceSkipReasons : reasonList([...surfaceSkipReasons, 'mutating_rest_method_requires_explicit_opt_in']);
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
			rest_mutation_opt_in: restMutationOptIn,
			auth: surface.auth || surface.authentication || surface.authorization || null,
			safety: safeMethod ? { level: 'safe', mutates: false } : { level: 'mutating', mutates: true, requires_explicit_opt_in: true, rollback_required: true },
			mutation_lifecycle: safeMethod ? undefined : mutationLifecycleContract({ kind: 'rest', surface, method }),
			isolation: safeMethod ? undefined : isolatedMutationMetadata({ mutationMode, kind: 'rest' }),
			reset: safeMethod ? undefined : isolatedResetMetadata({ kind: 'mutating_rest', mutationMode }),
			rollback_contract: safeMethod ? undefined : restMutationRollbackContract({ surface, method }),
			planned: !safeMethod,
			gated: !safeMethod,
		},
	}, { mutates: !safeMethod, executable: skipReasons.length === 0 && (safeMethod || isolatedMutationMode) });
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

function restRouteCasesFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options = {}) {
	const baseCase = restRouteCaseFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options);
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	if (mutationMode !== 'aggressive-isolated') {
		return [baseCase];
	}

	const variants = restArgDrivenVariants(surface, method);
	if (variants.length === 0) {
		return [baseCase];
	}

	return variants.map((variant) => restRouteCaseWithArgVariant(baseCase, surface, method, variant, options));
}

function restRouteCaseWithArgVariant(baseCase, surface, method, variant, options = {}) {
	const replay = stripUndefined({
		schema: 'homeboy/wordpress-rest-arg-driven-replay/v1',
		seed: options.seed,
		surface_id: surface.id,
		route: surface.route || surface.path,
		method,
		variant: variant.name,
		arg_names: variant.arg_names,
	});
	return {
		...baseCase,
		id: `${surface.id}-${method.toLowerCase()}-${safeIdPart(variant.name)}-arg-fuzz`,
		operation_id: `${baseCase.operation_id}:${safeIdPart(variant.name)}`,
		operation: bindRestArgVariantToOperation(baseCase.operation || {}, method, variant),
		metadata: stripUndefined({
			...(baseCase.metadata || {}),
			arg_generation: {
				schema: 'homeboy/wordpress-rest-arg-driven-case/v1',
				mode: 'aggressive-isolated',
				variant: variant.name,
				payload_family: variant.payload_family,
				arg_names: variant.arg_names,
				types: variant.types,
				max_payload_bounds: restPayloadBoundsForVariant(surface, variant),
			},
			replay,
			deterministic_seed: options.seed,
		}),
	};
}

function restPayloadBoundsForVariant(surface, variant) {
	const args = normalizeRestRouteArgs(surface.args || surface.arguments || surface.params || surface.parameters);
	const bounds = {};
	for (const name of variant.arg_names || []) {
		const arg = args.find((entry) => entry.name === name);
		if (arg) {
			bounds[name] = { bytes: restPayloadBound(arg, 'bytes'), items: restPayloadBound(arg, 'items'), depth: restPayloadBound(arg, 'depth') };
		}
	}
	return Object.keys(bounds).length > 0 ? bounds : undefined;
}

function bindRestArgVariantToOperation(operation, method, variant) {
	const payloadField = SAFE_REST_METHODS.has(method) ? 'query_params' : 'request_body';
	return stripUndefined({
		...operation,
		[payloadField]: {
			...(isObject(operation[payloadField]) ? operation[payloadField] : {}),
			...variant.values,
		},
	});
}

function restArgDrivenVariants(surface, method) {
	const args = normalizeRestRouteArgs(surface.args || surface.arguments || surface.params || surface.parameters);
	if (args.length === 0) {
		return [];
	}

	const variants = [];
	const requiredArgs = args.filter((arg) => arg.required);
	variants.push(restArgVariant('valid-minimal', Object.fromEntries(requiredArgs.map((arg) => [arg.name, sampleValueForRestArg(arg)])), requiredArgs));

	const boundaryArgs = args.filter((arg) => restArgTypes(arg).some((type) => ['string', 'array', 'object'].includes(type))).slice(0, 2);
	if (boundaryArgs.length > 0) {
		variants.push(restArgVariant('boundary-large', Object.fromEntries(boundaryArgs.map((arg) => [arg.name, boundaryValueForRestArg(arg)])), boundaryArgs, { payload_family: 'large' }));
	}
	for (const family of ['empty', 'null', 'enum', 'numeric', 'boolean', 'nested', 'repeated']) {
		const familyArgs = args.filter((arg) => payloadFamilySupportsRestArg(family, arg)).slice(0, 2);
		if (familyArgs.length > 0) {
			variants.push(restArgVariant(`payload-${family}`, Object.fromEntries(familyArgs.map((arg) => [arg.name, payloadFamilyValueForRestArg(family, arg)])), familyArgs, { payload_family: family }));
		}
	}

	const invalidArg = args.find((arg) => arg.name);
	if (invalidArg) {
		variants.push(restArgVariant('invalid-type', { [invalidArg.name]: invalidValueForRestArg(invalidArg) }, [invalidArg]));
	}

	return variants.filter((variant) => Object.keys(variant.values).length > 0).map((variant) => ({ ...variant, method }));
}

function restArgVariant(name, values, args, metadata = {}) {
	return {
		name,
		values,
		arg_names: args.map((arg) => arg.name).filter(Boolean),
		types: Object.fromEntries(args.map((arg) => [arg.name, restArgTypes(arg)]).filter(([name]) => Boolean(name))),
		...metadata,
	};
}

function normalizeRestRouteArgs(value) {
	if (Array.isArray(value)) {
		return value.map(normalizeRestRouteArg).filter(Boolean);
	}
	if (!isObject(value)) {
		return [];
	}
	if (Array.isArray(value.args)) {
		return value.args.map(normalizeRestRouteArg).filter(Boolean);
	}
	return Object.entries(value).map(([name, arg]) => normalizeRestRouteArg(isObject(arg) ? { name, ...arg } : { name, type: arg })).filter(Boolean);
}

function normalizeRestRouteArg(value) {
	if (!isObject(value)) {
		return null;
	}
	const name = value.name || value.arg || value.key || value.parameter || value.param;
	if (!name) {
		return null;
	}
	return { ...value, name: String(name), required: value.required === true };
}

function restArgTypes(arg = {}) {
	const rawType = arg.type || arg.types || arg.schema?.type || arg.items?.type;
	const values = Array.isArray(rawType) ? rawType : String(rawType || 'string').split('|');
	return [...new Set(values.map((type) => String(type).trim().toLowerCase()).filter(Boolean))];
}

function sampleValueForRestArg(arg) {
	const type = restArgTypes(arg)[0] || 'string';
	if (arg.default !== undefined) {
		return arg.default;
	}
	if (Array.isArray(arg.enum) && arg.enum.length > 0) {
		return arg.enum[0];
	}
	if (type === 'integer' || type === 'number') {
		return 1;
	}
	if (type === 'boolean') {
		return true;
	}
	if (type === 'array') {
		return ['sample'];
	}
	if (type === 'object') {
		return { sample: true };
	}
	return 'sample';
}

function boundaryValueForRestArg(arg) {
	if (restArgTypes(arg).includes('array')) {
		return Array.from({ length: restPayloadBound(arg, 'items') }, (_, index) => `item-${index + 1}`);
	}
	if (restArgTypes(arg).includes('object')) {
		return nestedPayloadValue(restPayloadBound(arg, 'depth'));
	}
	return 'x'.repeat(restPayloadBound(arg, 'bytes'));
}

function payloadFamilySupportsRestArg(family, arg) {
	const types = restArgTypes(arg);
	if (family === 'empty' || family === 'null' || family === 'repeated') {
		return true;
	}
	if (family === 'enum') {
		return Array.isArray(arg.enum) && arg.enum.length > 0;
	}
	if (family === 'numeric') {
		return types.some((type) => ['integer', 'number'].includes(type));
	}
	if (family === 'boolean') {
		return types.includes('boolean');
	}
	if (family === 'nested') {
		return types.some((type) => ['object', 'array'].includes(type));
	}
	return false;
}

function payloadFamilyValueForRestArg(family, arg) {
	const types = restArgTypes(arg);
	if (family === 'empty') {
		return types.includes('array') ? [] : (types.includes('object') ? {} : '');
	}
	if (family === 'null') {
		return null;
	}
	if (family === 'enum') {
		return arg.enum[arg.enum.length - 1];
	}
	if (family === 'numeric') {
		const explicitMax = Number(arg.maximum ?? arg.max ?? arg.max_value ?? arg.maxValue);
		if (Number.isFinite(explicitMax)) {
			return types.includes('integer') ? Math.floor(explicitMax) : explicitMax;
		}
		return types.includes('integer') ? 1 : 1.5;
	}
	if (family === 'boolean') {
		return false;
	}
	if (family === 'nested') {
		return nestedPayloadValue(restPayloadBound(arg, 'depth'));
	}
	if (family === 'repeated') {
		return Array.from({ length: restPayloadBound(arg, 'items') }, (_, index) => sampleValueForRestArg({ ...arg, default: undefined, enum: undefined, name: `${arg.name}-${index}` }));
	}
	return sampleValueForRestArg(arg);
}

function restPayloadBound(arg = {}, kind) {
	const bounds = arg.max_payload_bounds || arg.maxPayloadBounds || arg.bounds || {};
	const value = Number(bounds[kind] || bounds.max || arg[`max_${kind}`] || arg[`max${kind[0].toUpperCase()}${kind.slice(1)}`]);
	if (Number.isFinite(value) && value > 0) {
		return Math.floor(value);
	}
	return { bytes: 4096, items: 16, depth: 3 }[kind];
}

function nestedPayloadValue(depth) {
	let value = { leaf: 'sample' };
	for (let index = 0; index < depth; index += 1) {
		value = { level: index + 1, child: value, items: [value] };
	}
	return value;
}

function invalidValueForRestArg(arg) {
	const types = restArgTypes(arg);
	if (types.includes('string')) {
		return { invalid: 'object-for-string' };
	}
	if (types.includes('array')) {
		return 'not-an-array';
	}
	if (types.includes('integer') || types.includes('number')) {
		return 'not-a-number';
	}
	if (types.includes('boolean')) {
		return 'not-a-boolean';
	}
	if (types.includes('object')) {
		return 'not-an-object';
	}
	return { invalid: true };
}

function restMutationOptInForSurfaceAction(surface, method, options = {}) {
	const manifest = normalizeRestMutationOptInManifest(options.rest_mutation_opt_ins || options.restMutationOptIns || options.rest_mutation_opt_in || options.restMutationOptIn);
	const surfaceOptIn = normalizeRestMutationOptInEntry(surface.rest_mutation_opt_in || surface.restMutationOptIn || surface.mutation_opt_in || surface.mutationOptIn, { surface, method });
	if (surfaceOptIn) {
		return surfaceOptIn;
	}
	const normalizedMethod = String(method || '').toUpperCase();
	for (const entry of manifest.entries) {
		if (entry.allowed === false) {
			continue;
		}
		if (entry.method && entry.method !== normalizedMethod) {
			continue;
		}
		if ([entry.surface_id, entry.route, entry.path].filter(Boolean).some((value) => [surface.id, surface.route, surface.path, surface.name].includes(value))) {
			return stripUndefined({ ...entry, manifest_id: manifest.id });
		}
	}
	return undefined;
}

function normalizeRestMutationOptInManifest(value) {
	const source = Array.isArray(value) ? value : (isObject(value) ? value : {});
	const entries = (Array.isArray(source) ? source : source.entries || source.opt_ins || source.optIns || source.routes || [])
		.map((entry) => normalizeRestMutationOptInEntry(entry))
		.filter(Boolean);
	return { id: source.id || source.manifest_id || source.manifestId, entries };
}

function normalizeRestMutationOptInEntry(value, fallback = {}) {
	if (!isObject(value)) {
		return undefined;
	}
	const method = String(value.method || fallback.method || '').toUpperCase();
	return stripUndefined({
		id: value.id || value.opt_in_id || value.optInId,
		surface_id: value.surface_id || value.surfaceId || fallback.surface?.id,
		route: value.route || value.path || fallback.surface?.route || fallback.surface?.path,
		method: method || undefined,
		allowed: value.allowed !== false,
		fixture_ref: value.fixture_ref || value.fixtureRef,
		contract_ref: value.contract_ref || value.contractRef,
		metadata: isObject(value.metadata) ? value.metadata : undefined,
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
		const cases = blockCasesFromSurface(surface, operation, operationId, options);
		return wordpressFuzzAggressiveIsolatedMode(options) ? [...cases, randomWalkCaseFromSurface(surface, 'editor', options)] : cases;
	}
	if (['database-table', 'db-query'].includes(surface.type)) {
		const testCase = genericCaseForSurface(surface, options);
		return [testCase, ...dbQueryCasesFromSurface(surface, testCase.operation_id, options), ...dbMutationCasesFromSurface(surface, testCase.operation_id, options)];
	}
	const crudResource = crudResourceForSurface(surface);
	if (!crudResource) {
		const cases = [genericCaseForSurface(surface, options)];
		if (surface.type === 'frontend-url' && wordpressFuzzAggressiveIsolatedMode(options)) {
			cases.push(randomWalkCaseFromSurface(surface, 'browser', options));
		}
		return cases;
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

function randomWalkCaseFromSurface(surface, context, options = {}) {
	const seed = `${options.seed || 'wordpress-random-walk'}:${surface.id}:${context}`;
	const maxSteps = Number(options.random_walk_max_steps || options.randomWalkMaxSteps || 8);
	const actionFamilies = randomWalkActionFamilies(options.random_walk_action_families || options.randomWalkActionFamilies);
	const startUrl = randomWalkStartUrl(surface, context);
	const requiredCapabilities = context === 'editor' ? ['browser', 'block-editor'] : ['browser'];
	const walkMaxSteps = Number.isFinite(maxSteps) ? maxSteps : 8;
	const resetPolicy = randomWalkResetPolicy(surface, context, options);
	const input = randomWalkRuntimeActionInput({
		context,
		seed,
		max_steps: walkMaxSteps,
		action_families: actionFamilies,
		start_url: startUrl,
		reset_policy: resetPolicy,
	});
	const replay = stripUndefined({
		schema: 'wp-codebox/browser-random-walk/v1',
		seed,
		maxSteps: walkMaxSteps,
		actionFamilies,
		context,
		startUrl,
		resetPolicy: resetPolicy,
	});
	return gateWordPressFuzzCaseForRuntimeCapabilities({
		id: `${surface.id}-${context}-random-walk`,
		intent: `${context}-random-walk`,
		operation_id: `${surface.id}:${context}:random-walk`,
		operation: stripUndefined({ ...operationForSurface(surface), runtime_action: input.type, context, start_url: startUrl }),
		seed,
		target: wpCodeboxRuntimeActionTarget(input.type),
		input,
		required_capabilities: requiredCapabilities,
		skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
		destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
		metadata: stripUndefined({
			surface,
			random_walk: replay,
			replay,
			reset: replay.resetPolicy,
			runtime_action_type: input.type,
			safety: { mutation: 'bounded_random_user_actions', reset_required: true },
		}),
	}, options.runtimeCapabilities || options.runtime_capabilities, {
		required_capabilities: requiredCapabilities,
		mutation_mode: normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode),
		mutates: true,
	});
}

function randomWalkRuntimeActionInput(input = {}) {
	if (input.context === 'editor') {
		return stripUndefined({
			type: 'editor_open',
			url: input.start_url,
			capture: ['console', 'network'],
			metadata: randomWalkInputMetadata(input),
		});
	}
	if (input.context === 'admin') {
		return stripUndefined({
			type: 'admin_page',
			path: input.start_url || '/wp-admin/',
			capture: ['console', 'network'],
			metadata: randomWalkInputMetadata(input),
		});
	}
	return stripUndefined({
		type: 'browser_probe',
		url: input.start_url || '/',
		capture: ['console', 'network'],
		metadata: randomWalkInputMetadata(input),
	});
}

function randomWalkInputMetadata(input = {}) {
	return stripUndefined({
		random_walk: true,
		context: input.context,
		seed: input.seed,
		max_steps: input.max_steps,
		action_families: input.action_families,
		reset_policy: input.reset_policy,
	});
}

function wordpressFuzzAggressiveIsolatedMode(options = {}) {
	return normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode) === 'aggressive-isolated';
}

function randomWalkActionFamilies(value) {
	const allowed = ['click', 'fill', 'press', 'select', 'navigate', 'capture'];
	const families = (Array.isArray(value) ? value : []).map(String).filter((family) => allowed.includes(family));
	return families.length > 0 ? [...new Set(families)] : ['click', 'fill', 'press', 'select', 'navigate', 'capture'];
}

function randomWalkStartUrl(surface, context) {
	if (surface.url) {
		return surface.url;
	}
	if (surface.path) {
		return surface.path;
	}
	if (context === 'admin') {
		return '/wp-admin/';
	}
	if (context === 'editor') {
		return '/wp-admin/post-new.php';
	}
	return '/';
}

function randomWalkResetPolicy(surface, context, options = {}) {
	const explicit = surface.reset_policy || surface.resetPolicy || options.random_walk_reset_policy || options.randomWalkResetPolicy;
	if (isObject(explicit)) {
		return explicit;
	}
	return { mode: 'checkpoint-per-case', metadata: { context, surface_id: surface.id, reason: 'browser-random-walk' } };
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
			isolation: mutates ? isolatedMutationMetadata({ mutationMode, kind: restTransport ? 'rest_crud' : 'crud' }) : undefined,
			reset: mutates ? isolatedResetMetadata({ kind: restTransport ? 'rest_crud_mutation' : 'mutating_crud', mutationMode }) : undefined,
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

function isolatedMutationMetadata({ mutationMode, kind } = {}) {
	return stripUndefined({
		schema: 'homeboy/wordpress-fuzz-isolation/v1',
		mode: mutationMode || undefined,
		kind,
		boundary: 'per_case',
		required: true,
	});
}

function isolatedResetMetadata({ kind, mutationMode } = {}) {
	return stripUndefined({
		schema: 'homeboy/wordpress-fuzz-reset/v1',
		mode: mutationMode || undefined,
		strategy: 'checkpoint-restore-or-reset',
		boundary: 'after_each_case',
		required_capabilities: reasonList(requiredCapabilitiesForWordPressFuzzCase(kind)),
		required_any_capabilities: ['mutating_rest', 'rest_crud_mutation'].includes(kind) ? REST_ROLLBACK_ANY_CAPABILITIES.map((group) => [...group]) : undefined,
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

function objectHasValues(value) {
	return Boolean(value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length > 0);
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
	const generatedMutations = mutations.length > 0 ? [] : generatedDbMutationMetadata(surface, options);
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	return [...mutations, ...generatedMutations].map((mutation, index) => {
		const mutationId = mutation.id || mutation.name || mutation.operation || `mutation-${index + 1}`;
		return gateWordPressFuzzCaseForRuntimeCapabilities({
			id: `${surface.id}-${safeIdPart(mutationId)}-gated-mutation`,
			intent: surface.type === 'database-table' ? 'mutate-database-table' : 'mutate-database-query',
			operation_id: `${operationId}:${safeIdPart(mutationId)}`,
			operation: stripUndefined({
				...operationForSurface(surface),
				mutation: mutation.operation || mutation.name || mutation.type || mutationId,
				statement: mutation.statement || mutation.sql,
				where: mutation.where,
				values: mutation.values,
				limit: mutation.limit,
				columns: mutation.columns,
				options: mutation.options,
			}),
			target: wpCodeboxRuntimeActionTarget('php'),
			input: dbMutationRuntimeActionInput(surface, mutation),
			seed: options.seed,
			required_capabilities: DB_MUTATION_REQUIRED_CAPABILITIES,
			skip_reasons: [],
			destructive_reasons: ['db-mutation'],
			metadata: {
				surface,
				mutation,
				seed: mutation.seed,
				replay: mutation.replay,
				mutation_lifecycle: mutationLifecycleContract({ kind: 'database', surface }),
				isolation: isolatedMutationMetadata({ mutationMode, kind: 'database' }),
				reset: isolatedResetMetadata({ kind: 'db_mutation', mutationMode }),
				runtime_action_type: 'php',
			},
		}, options.runtimeCapabilities || options.runtime_capabilities, {
			required_capabilities: DB_MUTATION_REQUIRED_CAPABILITIES,
			mutation_mode: mutationMode,
			mutates: true,
		});
	});
}

function dbQueryCasesFromSurface(surface, operationId, options = {}) {
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	if (surface.type !== 'database-table' || mutationMode !== 'aggressive-isolated') {
		return [];
	}
	const keyColumn = dbPrimaryishColumn(surface);
	const valueColumn = dbWritableColumn(surface);
	if (!keyColumn && !valueColumn) {
		return [];
	}
	return [keyColumn, valueColumn]
		.filter(Boolean)
		.map((column) => annotateWordPressFuzzCaseExecutionTier({
			id: `${surface.id}-schema-query-${safeIdPart(column.name)}`,
			intent: 'profile-database-query',
			operation_id: `${operationId}:schema-query:${safeIdPart(column.name)}`,
			operation: stripUndefined({
				...operationForSurface(surface),
				query: `SELECT * FROM ${surface.table || surface.name || surface.id} WHERE ${column.name} = ? LIMIT 1`,
				where: { [column.name]: sampleValueForDbColumn(column) },
				limit: 1,
				options: { generated: true, bounded: true, source: 'schema-driven-db-query-generation' },
			}),
			target: wpCodeboxRuntimeActionTarget('php'),
			input: dbQueryRuntimeActionInput(surface, column),
			seed: options.seed,
			metadata: {
				surface,
				db_generation: { schema: 'homeboy/wordpress-db-query-generation/v1', source: 'table-column-metadata', column: column.name },
				replay: { source: 'schema-driven-db-query-generation', table: surface.table || surface.name || surface.id, column: column.name },
				runtime_action_type: 'php',
			},
		}, { mutates: false }));
}

function dbQueryRuntimeActionInput(surface, column) {
	const table = surface.table || surface.name || surface.id;
	return {
		type: 'php',
		code: `$wpdb->get_results( $wpdb->prepare( 'SELECT * FROM ${table} WHERE ${column.name} = %s LIMIT 1', ${JSON.stringify(String(sampleValueForDbColumn(column)))} ) );`,
		diagnostics: { capture: ['wpdb-queries'] },
	};
}

function dbMutationRuntimeActionInput(surface, mutation = {}) {
	const table = surface.table || surface.name || surface.id;
	const operation = String(mutation.operation || mutation.name || mutation.type || 'update').toLowerCase();
	const values = mutation.values || {};
	const where = mutation.where || {};
	return {
		type: 'php',
		code: dbMutationPhpCode({ table, operation, values, where, statement: mutation.statement || mutation.sql }),
		diagnostics: { capture: ['wpdb-queries'] },
	};
}

function dbMutationPhpCode({ table, operation, values, where, statement }) {
	if (statement) {
		return `$wpdb->query( ${JSON.stringify(String(statement))} );`;
	}
	if (operation === 'insert') {
		return `$wpdb->insert( ${JSON.stringify(table)}, ${JSON.stringify(values || {})} );`;
	}
	if (operation === 'delete') {
		return `$wpdb->delete( ${JSON.stringify(table)}, ${JSON.stringify(where || {})} );`;
	}
	return `$wpdb->update( ${JSON.stringify(table)}, ${JSON.stringify(values || {})}, ${JSON.stringify(where || {})} );`;
}

function generatedDbMutationMetadata(surface, options = {}) {
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	if (!['database-table', 'db-query'].includes(surface.type) || mutationMode !== 'aggressive-isolated' || !dbTableWritable(surface)) {
		return [];
	}
	const keyColumn = dbPrimaryishColumn(surface);
	const valueColumn = dbWritableColumn(surface);
	if (!keyColumn || !valueColumn) {
		return [];
	}
	return ['insert', 'update', 'delete'].map((operation) => stripUndefined({
		id: `schema-generated-${operation}`,
		operation,
		where: ['update', 'delete'].includes(operation) ? { [keyColumn.name]: sampleValueForDbColumn(keyColumn) } : undefined,
		values: ['insert', 'update'].includes(operation) ? { [valueColumn.name]: sampleValueForDbColumn(valueColumn) } : undefined,
		limit: ['update', 'delete'].includes(operation) ? 1 : undefined,
		options: { generated: true, bounded: true, reset_required: true },
		seed: { source: 'schema-driven-db-generation', table: surface.table || surface.name || surface.id, operation },
		replay: { source: 'schema-driven-db-generation', table: surface.table || surface.name || surface.id, operation, primary_key: keyColumn.name },
	}));
}

function statefulSequenceTargetsFromSurfaces(surfaces = [], options = {}) {
	if (!wordpressFuzzAggressiveIsolatedMode(options)) {
		return [];
	}
	const eligible = surfaces.filter(sequenceSurfaceSupported);
	if (eligible.length === 0) {
		return [];
	}
	const seed = options.seed || 'wordpress-stateful-sequence';
	const maxSteps = Number(options.stateful_sequence_max_steps || options.statefulSequenceMaxSteps || options.random_walk_max_steps || options.randomWalkMaxSteps || 8);
	const ordered = deterministicShuffle(eligible, seed).slice(0, Number.isFinite(maxSteps) && maxSteps > 0 ? maxSteps : 8);
	const steps = ordered.map((surface, index) => sequenceStepFromSurface(surface, index + 1));
	const input = stripUndefined({
		type: 'php',
		code: `$homeboy_stateful_sequence = ${JSON.stringify(steps)}; return $homeboy_stateful_sequence;`,
		diagnostics: { capture: ['wpdb-queries'] },
		sequence_type: 'stateful_sequence',
		seed,
		max_steps: steps.length,
		steps,
		reset_policy: { mode: 'checkpoint-per-sequence', required: true },
	});
	const replay = {
		schema: 'wp-codebox/stateful-sequence/v1',
		seed,
		maxSteps: steps.length,
		steps,
		resetPolicy: input.reset_policy,
	};
	return [{
		id: 'wordpress-stateful-sequence',
		surface_id: 'wordpress-stateful-sequence',
		type: 'stateful-sequence',
		operation_id: 'wordpress-stateful-sequence:random-walk',
		cases: [gateWordPressFuzzCaseForRuntimeCapabilities({
			id: 'wordpress-stateful-sequence-random-walk',
			intent: 'stateful-sequence',
			operation_id: 'wordpress-stateful-sequence:random-walk',
			operation: { runtime_action: 'php', sequence_type: 'stateful_sequence', steps },
			seed,
			target: wpCodeboxRuntimeActionTarget('php'),
			input,
			required_capabilities: ['sequence', 'snapshot', 'restore'],
			skip_reasons: [],
			destructive_reasons: ['stateful-sequence-may-mutate'],
			metadata: {
				sequence: replay,
				replay,
				reset: input.reset_policy,
				runtime_action_type: 'php',
				safety: { mutation: 'bounded_stateful_sequence', reset_required: true },
			},
		}, options.runtimeCapabilities || options.runtime_capabilities, {
			required_capabilities: ['sequence', 'snapshot', 'restore'],
			mutation_mode: normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode),
			mutates: true,
		})],
		metadata: { type: 'stateful-sequence', sequence: replay, execution_tiers: {} },
	}];
}

function sequenceSurfaceSupported(surface = {}) {
	return ['rest-route', 'admin-page', 'frontend-url', 'block', 'database-table', 'db-query'].includes(surface.type);
}

function sequenceStepFromSurface(surface, index) {
	const family = {
		'rest-route': 'rest',
		'admin-page': 'admin',
		'frontend-url': 'browser',
		block: 'editor',
		'database-table': 'database',
		'db-query': 'database',
	}[surface.type] || 'surface';
	return stripUndefined({
		index,
		family,
		surface_id: surface.id,
		type: surface.type,
		method: restMethodsForSurface(surface)[0] || surface.method,
		route: surface.route,
		path: surface.path || surface.url,
		block_name: surface.block_name || surface.blockName,
		table: surface.table,
		query: surface.query,
	});
}

function deterministicShuffle(items, seed) {
	return [...items].sort((left, right) => deterministicNumber(`${seed}:${left.id}`) - deterministicNumber(`${seed}:${right.id}`));
}

function deterministicNumber(value) {
	let hash = 2166136261;
	for (const char of String(value)) {
		hash ^= char.charCodeAt(0);
		hash = Math.imul(hash, 16777619);
	}
	return hash >>> 0;
}

function dbTableWritable(surface) {
	return surface.writable !== false && surface.read_only !== true && surface.readOnly !== true;
}

function dbPrimaryishColumn(surface) {
	const columns = normalizeDbColumns(surface.columns || surface.schema?.columns);
	const indexes = normalizeDbIndexes(surface.indexes || surface.schema?.indexes);
	const primaryNames = normalizeArray(surface.primary_key_columns || surface.primaryKeyColumns)
		.map((entry) => String(entry || '').trim())
		.filter(Boolean);
	const indexedNames = indexes.filter((index) => index.name === 'PRIMARY' || index.unique === true).sort((a, b) => (a.sequence || 0) - (b.sequence || 0)).map((index) => index.column);
	return columns.find((column) => [...primaryNames, ...indexedNames].includes(column.name)) || columns.find((column) => ['PRI', 'UNI'].includes(column.key));
}

function dbWritableColumn(surface) {
	const columns = normalizeDbColumns(surface.columns || surface.schema?.columns);
	return columns.find((column) => !/auto_increment/i.test(column.extra) && column.key !== 'PRI') || columns.find((column) => !/auto_increment/i.test(column.extra));
}

function normalizeDbColumns(value) {
	if (!Array.isArray(value)) {
		return [];
	}
	return value.map((column) => isObject(column) ? {
		name: String(column.name || column.column || '').trim(),
		type: String(column.type || column.column_type || column.columnType || '').toLowerCase(),
		key: String(column.key || column.column_key || column.columnKey || '').toUpperCase(),
		extra: String(column.extra || ''),
	} : null).filter((column) => column?.name);
}

function normalizeDbIndexes(value) {
	if (!Array.isArray(value)) {
		return [];
	}
	return value.map((index) => isObject(index) ? {
		name: String(index.name || index.index || index.key_name || index.keyName || '').trim(),
		column: String(index.column || index.column_name || index.columnName || '').trim(),
		unique: index.unique === true || index.non_unique === 0 || index.nonUnique === 0,
		sequence: Number(index.sequence || index.seq_in_index || index.seqInIndex || 0),
	} : null).filter((index) => index?.name && index?.column);
}

function sampleValueForDbColumn(column) {
	const type = String(column.type || '').toLowerCase();
	if (type.includes('int') || type.includes('decimal') || type.includes('float') || type.includes('double')) {
		return 1;
	}
	if (type.includes('bool')) {
		return true;
	}
	if (type.includes('date') || type.includes('time')) {
		return '2000-01-01 00:00:00';
	}
	return 'homeboy-fuzz-sample';
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
	if (gated && !wordpressFuzzMutationModeAllowsIsolatedExecution(mutationMode)) {
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
			isolation: gated ? isolatedMutationMetadata({ mutationMode, kind: 'admin' }) : undefined,
			reset: gated ? isolatedResetMetadata({ kind: 'admin_mutation', mutationMode }) : undefined,
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

function adminPageActionDiscoveryCase(surface, options = {}) {
	const operation = stripUndefined({
		...operationForSurface(surface),
		discovery: 'admin-page-actions',
		method: surface.method || 'GET',
	});
	return annotateWordPressFuzzCaseExecutionTier({
		id: `${surface.id}-discover-admin-actions`,
		intent: 'discover-admin-page-actions',
		operation_id: `${surface.id}:discover-admin-actions`,
		operation,
		seed: options.seed,
		skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
		destructive_reasons: [],
		metadata: stripUndefined({
			surface,
			action_discovery: { schema: 'homeboy/wordpress-admin-action-discovery-case/v1', executes_actions: false, forms_discovered: false },
			safety: { level: 'safe', mutates: false },
			executable: true,
		}),
	}, { mutates: false });
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

function wordpressFuzzPlanDiagnostics(targets) {
	const diagnostics = [];
	for (const target of targets) {
		if (target.type === 'database-table' && !target.cases?.some((testCase) => testCase.intent === 'mutate-database-table')) {
			diagnostics.push({
				code: 'wordpress-db-schema-driven-generation-unavailable',
				message: 'Database table schema metadata did not declare executable mutation cases; planner did not synthesize fake DB mutations.',
				surface_id: target.surface_id || target.id,
			});
		}
		if (target.type === 'rest-route' && !target.cases?.some((testCase) => testCase.metadata?.arg_generation)) {
			diagnostics.push({
				code: 'wordpress-rest-arg-generation-unavailable',
				message: 'REST route argument metadata was missing or empty; planner did not synthesize payload-family cases.',
				surface_id: target.surface_id || target.id,
			});
		}
		if (target.type === 'admin-page' && !target.cases?.some((testCase) => testCase.intent === 'plan-admin-page-mutation')) {
			diagnostics.push({
				code: 'wordpress-admin-interaction-generation-unavailable',
				message: 'Admin interaction metadata did not declare forms/actions; planner kept the admin page as a page-load case only.',
				surface_id: target.surface_id || target.id,
			});
		}
	}
	return diagnostics.length > 0 ? diagnostics : undefined;
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

function normalizeArray(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return Array.isArray(value) ? value : [value];
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
