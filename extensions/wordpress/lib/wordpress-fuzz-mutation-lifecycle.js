'use strict';

const WORDPRESS_FUZZ_MUTATION_LIFECYCLE_SCHEMA = 'homeboy/wordpress-fuzz-mutation-lifecycle/v1';

const LIFECYCLE_REQUIRED_CAPABILITIES = Object.freeze({
	crud: Object.freeze(['crud', 'snapshot', 'restore', 'reset']),
	rest: Object.freeze(['rest', 'checkpoint', 'rest-rollback']),
	rest_crud: Object.freeze(['crud', 'rest', 'checkpoint', 'rest-rollback']),
	admin: Object.freeze(['admin', 'snapshot', 'restore', 'reset']),
	database: Object.freeze(['database', 'snapshot', 'transaction', 'reset']),
});

const LIFECYCLE_REQUIRED_ANY_CAPABILITIES = Object.freeze({
	rest: Object.freeze([Object.freeze(['restore', 'reset'])]),
	rest_crud: Object.freeze([Object.freeze(['restore', 'reset'])]),
});

function buildWordPressFuzzMutationLifecycleContract(input = {}) {
	const kind = normalizeLifecycleKind(input.kind || input.surface_kind || input.surfaceKind || input.type);
	const action = stringOrUndefined(input.action);
	const method = stringOrUndefined(input.method);
	const deleteBoundary = input.delete_boundary === true || input.deleteBoundary === true || action === 'delete' || String(method || '').toUpperCase() === 'DELETE';
	return normalizeWordPressFuzzMutationLifecycleContract({
		schema: WORDPRESS_FUZZ_MUTATION_LIFECYCLE_SCHEMA,
		kind,
		action,
		method,
		rollback_boundary: input.rollback_boundary || input.rollbackBoundary || 'after_each_case',
		required_capabilities: input.required_capabilities || input.requiredCapabilities || LIFECYCLE_REQUIRED_CAPABILITIES[kind],
		required_any_capabilities: input.required_any_capabilities || input.requiredAnyCapabilities || LIFECYCLE_REQUIRED_ANY_CAPABILITIES[kind],
		required_evidence: input.required_evidence || input.requiredEvidence || defaultLifecycleEvidence(kind, { deleteBoundary }),
		required_any_evidence: input.required_any_evidence || input.requiredAnyEvidence || defaultLifecycleAnyEvidence(kind),
		delete_boundary_required: deleteBoundary,
		metadata: input.metadata,
	});
}

function normalizeWordPressFuzzMutationLifecycleContract(contract) {
	if (!contract) {
		return undefined;
	}
	assertPlainObject(contract, 'mutation_lifecycle');
	if (contract.schema && contract.schema !== WORDPRESS_FUZZ_MUTATION_LIFECYCLE_SCHEMA) {
		throw new Error(`Unsupported WordPress fuzz mutation lifecycle schema: ${contract.schema}`);
	}
	const kind = normalizeLifecycleKind(contract.kind || contract.type);
	return stripUndefined({
		schema: WORDPRESS_FUZZ_MUTATION_LIFECYCLE_SCHEMA,
		kind,
		action: stringOrUndefined(contract.action),
		method: stringOrUndefined(contract.method),
		rollback_boundary: stringOrUndefined(contract.rollback_boundary || contract.rollbackBoundary) || 'after_each_case',
		required_capabilities: uniqueStrings(contract.required_capabilities || contract.requiredCapabilities),
		required_any_capabilities: normalizeCapabilityGroups(contract.required_any_capabilities || contract.requiredAnyCapabilities),
		required_evidence: normalizeLifecycleEvidence(contract.required_evidence || contract.requiredEvidence),
		required_any_evidence: normalizeLifecycleEvidenceGroups(contract.required_any_evidence || contract.requiredAnyEvidence),
		delete_boundary_required: contract.delete_boundary_required ?? contract.deleteBoundaryRequired ?? false,
		metadata: objectOrUndefined(contract.metadata),
	});
}

function wordpressFuzzMutationLifecycleDiagnosticsForCase(testCase = {}, artifacts = []) {
	const contract = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle);
	if (!contract || !caseClaimsMutationExecution(testCase)) {
		return [];
	}
	const missing = [
		...contract.required_evidence.filter((requirement) => !hasLifecycleEvidence(testCase, artifacts, requirement)),
		...contract.required_any_evidence.filter((group) => !group.some((requirement) => hasLifecycleEvidence(testCase, artifacts, requirement))).map((group) => ({ any_of: group })),
	];
	if (missing.length === 0) {
		return [];
	}
	return [{
		severity: 'error',
		code: 'wordpress_fuzz_mutation_lifecycle_evidence_missing',
		message: 'Executed WordPress mutation case is missing required sandbox rollback lifecycle evidence.',
		case_id: testCase.id || testCase.case_id || testCase.caseId,
		missing_evidence: missing,
		contract,
	}];
}

function caseClaimsMutationExecution(testCase = {}) {
	const status = String(testCase.status || '').toLowerCase();
	return status !== 'skipped' && status !== 'skip' && status !== 'planned' && status !== 'plan_only';
}

function hasLifecycleEvidence(testCase = {}, artifacts = [], requirement = {}) {
	const candidates = [
		...arrayOf(testCase.lifecycle_evidence || testCase.lifecycleEvidence),
		...arrayOf(testCase.rollback_evidence || testCase.rollbackEvidence),
		...arrayOf(testCase.evidence),
		...arrayOf(testCase.artifacts || testCase.artifactRefs || testCase.artifact_refs),
		...arrayOf(testCase.metadata?.lifecycle_evidence || testCase.metadata?.lifecycleEvidence),
		...arrayOf(testCase.metadata?.rollback_evidence || testCase.metadata?.rollbackEvidence),
		...arrayOf(testCase.metadata?.evidence),
		...arrayOf(testCase.metadata?.artifacts || testCase.metadata?.artifactRefs || testCase.metadata?.artifact_refs),
		...arrayOf(artifacts),
	];
	const rollbackResult = testCase.rollback_result || testCase.rollbackResult;
	if (requirement.kind === 'restore' && rollbackResult && !['failed', 'errored', 'error', 'missing'].includes(String(rollbackResult.status || '').toLowerCase())) {
		return true;
	}
	return candidates.some((candidate) => evidenceMatchesRequirement(candidate, requirement, testCase));
}

function evidenceMatchesRequirement(candidate, requirement = {}, testCase = {}) {
	if (!candidate || typeof candidate !== 'object' || Array.isArray(candidate)) {
		return false;
	}
	const status = String(candidate.status || candidate.outcome || '').toLowerCase();
	if (['failed', 'errored', 'error', 'missing'].includes(status)) {
		return false;
	}
	const caseId = candidate.case_id || candidate.caseId;
	if (caseId && testCase.id && caseId !== testCase.id) {
		return false;
	}
	const labels = [
		candidate.kind,
		candidate.type,
		candidate.role,
		candidate.name,
		candidate.semantic_key,
		candidate.semanticKey,
		candidate.metadata?.kind,
		candidate.metadata?.role,
		candidate.metadata?.semantic_key,
		candidate.metadata?.semanticKey,
	].map((value) => String(value || '').toLowerCase().replace(/[\s._]+/g, '-'));
	return labels.includes(String(requirement.kind || '').toLowerCase())
		|| labels.includes(String(requirement.semantic_key || requirement.semanticKey || '').toLowerCase())
		|| labels.some((label) => label.includes(String(requirement.kind || '').toLowerCase()));
}

function defaultLifecycleEvidence(kind, { deleteBoundary = false } = {}) {
	const evidence = [];
	if (['crud', 'admin'].includes(kind)) {
		evidence.push(lifecycleEvidence('snapshot'), lifecycleEvidence('restore'), lifecycleEvidence('reset'));
	}
	if (['rest', 'rest_crud'].includes(kind)) {
		evidence.push(lifecycleEvidence('checkpoint'));
	}
	if (kind === 'database') {
		evidence.push(lifecycleEvidence('snapshot'), lifecycleEvidence('transaction'), lifecycleEvidence('reset'));
	}
	if (deleteBoundary) {
		evidence.push(lifecycleEvidence('delete-boundary', 'fuzz.rollback.delete_boundary'));
	}
	return evidence;
}

function defaultLifecycleAnyEvidence(kind) {
	if (['rest', 'rest_crud'].includes(kind)) {
		return [[lifecycleEvidence('restore'), lifecycleEvidence('reset')]];
	}
	return [];
}

function lifecycleEvidence(kind, semanticKey) {
	return stripUndefined({ kind, semantic_key: semanticKey || `fuzz.lifecycle.${kind}`, required: true });
}

function normalizeLifecycleEvidence(value) {
	return arrayOf(value).map((entry) => {
		if (typeof entry === 'string') {
			return lifecycleEvidence(entry);
		}
		assertPlainObject(entry, 'mutation_lifecycle.required_evidence[]');
		return stripUndefined({
			kind: stringOrUndefined(entry.kind || entry.type) || 'evidence',
			semantic_key: stringOrUndefined(entry.semantic_key || entry.semanticKey),
			required: entry.required !== false,
			metadata: objectOrUndefined(entry.metadata),
		});
	}).filter((entry) => entry.required !== false);
}

function normalizeLifecycleEvidenceGroups(value) {
	return arrayOf(value).map((group) => normalizeLifecycleEvidence(group)).filter((group) => group.length > 0);
}

function normalizeLifecycleKind(value) {
	const kind = String(value || 'crud').trim().toLowerCase().replace(/[\s-]+/g, '_');
	if (['rest_crud', 'rest', 'crud', 'admin', 'database'].includes(kind)) {
		return kind;
	}
	return 'crud';
}

function normalizeCapabilityGroups(value) {
	return arrayOf(value).map((group) => uniqueStrings(group)).filter((group) => group.length > 0);
}

function uniqueStrings(value) {
	return [...new Set(arrayOf(value).map(String).filter(Boolean))].sort();
}

function arrayOf(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return Array.isArray(value) ? value : [value];
}

function stringOrUndefined(value) {
	return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function assertPlainObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	WORDPRESS_FUZZ_MUTATION_LIFECYCLE_SCHEMA,
	buildWordPressFuzzMutationLifecycleContract,
	normalizeWordPressFuzzMutationLifecycleContract,
	wordpressFuzzMutationLifecycleDiagnosticsForCase,
};
