'use strict';

const FUZZ_WORKLOAD_SCHEMA = 'homeboy/fuzz-workload/v1';
const FUZZ_CASE_INTENT_SCHEMA = 'homeboy/fuzz-workload-intent/v1';
const FUZZ_READINESS_LEVELS = new Set(['declared', 'executable', 'proven']);
const FUZZ_CRUD_OPERATIONS = new Set(['create', 'read', 'update', 'delete']);
const FUZZ_PROOF_BUNDLE_FIELDS = new Set(['artifact_refs', 'run_ids', 'gap_reports', 'fuzz_result_artifacts']);
const FUZZ_SAFETY_CLASSES = new Set(['read_only', 'idempotent', 'isolated_mutation', 'destructive']);

function assertGenericFuzzWorkload(manifest, options = {}) {
	const issues = collectGenericFuzzWorkloadIssues(manifest, options);
	if (issues.length > 0) {
		throw new Error(issues[0]);
	}
	return manifest;
}

function collectGenericFuzzWorkloadIssues(manifest, options = {}) {
	const context = options.context || manifest?.id || 'fuzz workload';
	const issues = [];

	if (!isPlainObject(manifest)) {
		return [`${context} must be an object`];
	}

	if (manifest.schema !== FUZZ_WORKLOAD_SCHEMA) {
		issues.push(`${context} must use schema ${FUZZ_WORKLOAD_SCHEMA}`);
	}

	for (const field of ['id', 'label', 'safety_class']) {
		if (!isNonEmptyString(manifest[field])) {
			issues.push(`${context} must declare a non-empty string ${field}`);
		}
	}

	if (isNonEmptyString(manifest.safety_class) && !FUZZ_SAFETY_CLASSES.has(manifest.safety_class)) {
		issues.push(`${context} safety_class must be one of ${[...FUZZ_SAFETY_CLASSES].join(', ')}`);
	}

	if (!isPlainObject(manifest.metadata)) {
		issues.push(`${context} must declare metadata`);
	}

	if (!isPlainObject(manifest.target)) {
		issues.push(`${context} must declare target`);
	}

	if (!isPlainObject(manifest.workload)) {
		issues.push(`${context} must declare workload`);
	} else {
		if (!isNonEmptyString(manifest.workload.path)) {
			issues.push(`${context} workload must declare a non-empty string path`);
		}
		if (manifest.workload.type !== undefined && !isNonEmptyString(manifest.workload.type)) {
			issues.push(`${context} workload.type must be a non-empty string when declared`);
		}
		if (manifest.workload.runner !== undefined && !isNonEmptyString(manifest.workload.runner)) {
			issues.push(`${context} workload.runner must be a non-empty string when declared`);
		}
	}

	if (!Array.isArray(manifest.cases) || manifest.cases.length === 0) {
		issues.push(`${context} must declare at least one case`);
	} else {
		for (const runnerCase of manifest.cases) {
			const caseId = runnerCase?.case_id || '(unknown)';
			if (runnerCase?.metadata?.safety_class && runnerCase.metadata.safety_class !== manifest.safety_class) {
				issues.push(`${context} case ${caseId} metadata.safety_class must match workload safety_class ${manifest.safety_class}`);
			}

			if (runnerCase?.intent) {
				try {
					assertRunnerNeutralFuzzCaseIntent(manifest, runnerCase);
				} catch (error) {
					issues.push(`${context} ${error.message}`);
				}
			}
		}
	}

	return issues;
}

function assertFuzzReadinessMetadata(manifest, options = {}) {
	const file = options.file || manifest?.id || 'fuzz workload';
	const readiness = manifest?.metadata?.readiness;
	assertPlainObject(readiness, `${file} requires metadata.readiness`);

	assertFuzzReadinessLevel(readiness.level, `${file} metadata.readiness.level`);
	if (!isNonEmptyString(readiness.coverage_contract)) {
		throw new Error(`${file} metadata.readiness.coverage_contract must describe the declared contract`);
	}

	if (readiness.proof_refs !== undefined) {
		assertStringArray(readiness.proof_refs, `${file} metadata.readiness.proof_refs`);
		for (const proofRef of readiness.proof_refs) {
			assertReviewerFacingRef(proofRef, `${file} metadata.readiness.proof_refs`);
		}
	}

	if (readiness.level === 'proven') {
		if (!Array.isArray(readiness.proof_refs) || readiness.proof_refs.length === 0) {
			throw new Error(`${file} proven readiness requires proof_refs`);
		}
		assertFuzzProofBundle(readiness.proof_bundle, manifest, { file });
	}

	if (readiness.proof_bundle_requirements !== undefined) {
		assertFuzzProofBundleRequirements(readiness.proof_bundle_requirements, { file });
	}

	if (readiness.upstream_blockers !== undefined) {
		assertStringArray(readiness.upstream_blockers, `${file} metadata.readiness.upstream_blockers`, { allowEmpty: true });
	}

	if (readiness.crud !== undefined) {
		assertFuzzCrudReadiness(readiness.crud, { file });
	}

	if (readiness.mutation !== undefined) {
		assertFuzzMutationReadiness(readiness.mutation, { file });
	}

	return readiness;
}

function assertRunnerNeutralFuzzCaseIntent(manifest, runnerCase) {
	const manifestId = manifest?.id || 'fuzz workload';
	const intent = runnerCase?.intent;
	assertPlainObject(intent, `${manifestId} case intent must be an object`);
	assertEqual(intent.schema, FUZZ_CASE_INTENT_SCHEMA, `${manifestId} case intent schema mismatch`);
	assertEqual(intent.type, 'wordpress-plugin-workload', `${manifestId} case intent type mismatch`);

	assertPlainObject(intent.plugin, `${manifestId} case intent requires plugin`);
	if (!isNonEmptyString(intent.plugin.activation)) {
		throw new Error(`${manifestId} case intent plugin.activation must be a string`);
	}

	assertPlainObject(intent.execute, `${manifestId} case intent requires execute`);
	assertEqual(intent.execute.workload_ref, 'default', `${manifestId} case intent execute.workload_ref must be default`);
	assertEqual(intent.execute.path, manifest?.workload?.path, `${manifestId} case intent execute.path must match workload.path`);
	assertEqual(intent.execute.type, manifest?.workload?.type, `${manifestId} case intent execute.type must match workload.type`);
	if (manifest?.workload?.entry) {
		assertEqual(intent.execute.entry, manifest.workload.entry, `${manifestId} case intent execute.entry must match workload.entry`);
	}
	if (intent.execute.parameters !== undefined) {
		assertPlainObject(intent.execute.parameters, `${manifestId} case intent execute.parameters must be an object`);
	}

	if (!Array.isArray(intent.collect)) {
		throw new Error(`${manifestId} case intent collect must be an array`);
	}
	if (intent.collect.length === 0) {
		throw new Error(`${manifestId} case intent collect must declare at least one artifact`);
	}

	const caseArtifactNames = new Set((runnerCase.artifacts || []).map((artifact) => artifact?.name).filter(Boolean));
	for (const artifact of intent.collect) {
		assertPlainObject(artifact, `${manifestId} case intent collect entries must be objects`);
		if (!isNonEmptyString(artifact.artifact)) {
			throw new Error(`${manifestId} case intent collect artifact must be a string`);
		}
		if (!caseArtifactNames.has(artifact.artifact)) {
			throw new Error(`${manifestId} case intent collect artifact ${artifact.artifact} is not declared on the case`);
		}
	}

	if (runnerCase.phases !== undefined) {
		throw new Error(`${manifestId} runner-neutral case intent must not embed runner command phases`);
	}

	return intent;
}

function assertFuzzProofBundle(proofBundle, manifest, options = {}) {
	const file = options.file || manifest?.id || 'fuzz workload';
	assertPlainObject(proofBundle, `${file} proven readiness requires proof_bundle`);

	for (const field of FUZZ_PROOF_BUNDLE_FIELDS) {
		assertStringArray(proofBundle[field], `${file} metadata.readiness.proof_bundle.${field}`);
		for (const value of proofBundle[field]) {
			if (field !== 'fuzz_result_artifacts') {
				assertReviewerFacingRef(value, `${file} metadata.readiness.proof_bundle.${field}`);
			}
		}
	}

	const requiredArtifactNames = collectRequiredArtifactNames(manifest);
	for (const artifactName of proofBundle.fuzz_result_artifacts) {
		if (!requiredArtifactNames.has(artifactName)) {
			throw new Error(`${file} proof_bundle.fuzz_result_artifacts ${artifactName} must name a required case or expected artifact`);
		}
	}
}

function assertFuzzCrudReadiness(crud, options = {}) {
	const file = options.file || 'fuzz workload';
	assertPlainObject(crud, `${file} metadata.readiness.crud must be an object`);

	for (const operation of FUZZ_CRUD_OPERATIONS) {
		assertPlainObject(crud[operation], `${file} metadata.readiness.crud.${operation} must be an object`);
		assertFuzzReadinessLevel(crud[operation].level, `${file} metadata.readiness.crud.${operation}.level`);
		if (crud[operation].upstream_blocker !== undefined && !isNonEmptyString(crud[operation].upstream_blocker)) {
			throw new Error(`${file} metadata.readiness.crud.${operation}.upstream_blocker must be non-empty`);
		}
	}
}

function assertFuzzProofBundleRequirements(requirements, options = {}) {
	const file = options.file || 'fuzz workload';
	assertPlainObject(requirements, `${file} metadata.readiness.proof_bundle_requirements must be an object`);
	assertStringArray(requirements.required_refs, `${file} metadata.readiness.proof_bundle_requirements.required_refs`);
	assertStringArray(requirements.required_artifacts, `${file} metadata.readiness.proof_bundle_requirements.required_artifacts`);
	if (requirements.status !== undefined) {
		assertEqual(requirements.status, 'required_before_proven', `${file} metadata.readiness.proof_bundle_requirements.status must be required_before_proven`);
	}
}

function assertFuzzMutationReadiness(mutation, options = {}) {
	const file = options.file || 'fuzz workload';
	assertPlainObject(mutation, `${file} metadata.readiness.mutation must be an object`);
	if (!isNonEmptyString(mutation.safety_boundary)) {
		throw new Error(`${file} metadata.readiness.mutation.safety_boundary must describe rollback/isolation boundaries`);
	}
	assertStringArray(mutation.rollback_artifacts, `${file} metadata.readiness.mutation.rollback_artifacts`, { allowEmpty: true });
}

function assertFuzzReadinessLevel(level, label) {
	if (!FUZZ_READINESS_LEVELS.has(level)) {
		throw new Error(`${label} must be declared, executable, or proven`);
	}
}

function assertReviewerFacingRef(value, context) {
	if (!/^(https:\/\/|gh:|homeboy-runs:|artifact:|run:)/.test(value)) {
		throw new Error(`${context} entries must be reviewer-facing refs`);
	}
	if (/^(https?:\/\/)?(localhost|127\.0\.0\.1)([:/]|$)/.test(value)) {
		throw new Error(`${context} entries must not use local URLs`);
	}
	if (value.startsWith('/Users/')) {
		throw new Error(`${context} entries must not use local filesystem paths`);
	}
}

function collectRequiredArtifactNames(manifest) {
	return new Set([
		...(manifest?.cases || []).flatMap((runnerCase) => runnerCase.artifacts || []),
		...(manifest?.artifacts?.expected || []),
	]
		.filter((artifact) => artifact?.required === true)
		.map((artifact) => artifact?.name)
		.filter(Boolean));
}

function assertStringArray(value, label, options = {}) {
	if (!Array.isArray(value)) {
		throw new Error(`${label} must be an array`);
	}
	if (!options.allowEmpty && value.length === 0) {
		throw new Error(`${label} must not be empty`);
	}
	for (const entry of value) {
		if (!isNonEmptyString(entry)) {
			throw new Error(`${label} entries must be non-empty strings`);
		}
	}
}

function assertPlainObject(value, message) {
	if (!isPlainObject(value)) {
		throw new Error(message);
	}
}

function assertEqual(actual, expected, message) {
	if (actual !== expected) {
		throw new Error(message);
	}
}

function isPlainObject(value) {
	return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function isNonEmptyString(value) {
	return typeof value === 'string' && value.trim() !== '';
}

module.exports = {
	FUZZ_WORKLOAD_SCHEMA,
	FUZZ_CASE_INTENT_SCHEMA,
	FUZZ_READINESS_LEVELS,
	FUZZ_CRUD_OPERATIONS,
	FUZZ_PROOF_BUNDLE_FIELDS,
	FUZZ_SAFETY_CLASSES,
	assertGenericFuzzWorkload,
	assertFuzzReadinessLevel,
	assertFuzzReadinessMetadata,
	assertFuzzProofBundle,
	assertFuzzProofBundleRequirements,
	assertFuzzCrudReadiness,
	assertFuzzMutationReadiness,
	assertReviewerFacingRef,
	assertRunnerNeutralFuzzCaseIntent,
	collectGenericFuzzWorkloadIssues,
};
