'use strict';

const WP_CODEBOX_WORKSPACE_RECIPE_SCHEMA = 'wp-codebox/workspace-recipe/v1';
const WP_CODEBOX_FUZZ_SUITE_SCHEMA = 'wp-codebox/fuzz-suite/v1';
const WORDPRESS_FUZZ_PLAN_RECIPE_BUILDER_SCHEMA = 'homeboy/wordpress-fuzz-plan-recipe-builder/v1';
const DEFAULT_WORKFLOW_STEP = { command: 'inspect-mounted-inputs' };
const FUZZ_PHASES = ['setup', 'action', 'assert', 'teardown'];

function buildWpCodeboxFuzzPlanRecipe(input = {}) {
	const plan = input.plan || input.fuzzPlan || input.fuzz_plan || input;
	if (!plan || typeof plan !== 'object' || Array.isArray(plan)) {
		throw new Error('buildWpCodeboxFuzzPlanRecipe requires a WordPress fuzz plan object.');
	}

	const cases = normalizeFuzzCases(plan.cases || plan.fuzzCases || plan.fuzz_cases);
	const workflowSteps = normalizeWorkflowSteps(
		plan.workflowSteps || plan.workflow_steps || plan.workflow?.steps || input.workflowSteps || input.workflow_steps
	);

	return stripUndefined({
		schema: WP_CODEBOX_WORKSPACE_RECIPE_SCHEMA,
		inputs: objectWithEntries({
			mounts: normalizeMounts(plan.mounts),
			extraPlugins: normalizeArray(plan.extraPlugins || plan.extra_plugins),
			env: objectOrUndefined(plan.env),
			wpConfigDefines: objectOrUndefined(plan.wpConfigDefines || plan.wp_config_defines),
		}),
		runtime: objectOrUndefined(plan.runtime),
		workflow: { steps: workflowSteps },
		fuzzSuite: stripUndefined({
			schema: WP_CODEBOX_FUZZ_SUITE_SCHEMA,
			cases,
			metadata: objectWithEntries({
				...(plan.metadata || {}),
				planner: WORDPRESS_FUZZ_PLAN_RECIPE_BUILDER_SCHEMA,
				plan_id: plan.planId || plan.plan_id || plan.id,
			}),
		}),
	});
}

function normalizeFuzzCases(cases) {
	const normalizedCases = normalizeArray(cases).map((entry, index) => normalizeFuzzCase(entry, index));
	if (normalizedCases.length === 0) {
		throw new Error('WordPress fuzz plan requires at least one case.');
	}
	return normalizedCases;
}

function normalizeFuzzCase(entry = {}, index = 0) {
	if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
		throw new Error(`WordPress fuzz plan case ${index} must be an object.`);
	}

	const caseId = entry.case_id || entry.caseId || entry.id;
	if (typeof caseId !== 'string' || caseId.trim() === '') {
		throw new Error(`WordPress fuzz plan case ${index} requires case_id.`);
	}

	const phases = normalizePhases(entry.phases || entry, caseId);
	if (!Array.isArray(phases.action) || phases.action.length === 0) {
		throw new Error(`WordPress fuzz plan case ${caseId} requires at least one action phase step.`);
	}

	return stripUndefined({
		case_id: caseId,
		input: objectOrUndefined(entry.input),
		inputHash: objectOrUndefined(entry.inputHash || entry.input_hash),
		metadata: objectOrUndefined(entry.metadata),
		phases,
		artifacts: normalizeDeclaredArtifacts(entry.artifacts),
		replay: objectOrUndefined(entry.replay),
	});
}

function normalizePhases(source = {}, caseId = '') {
	const phases = {};
	for (const phase of FUZZ_PHASES) {
		const steps = normalizeRecipeSteps(source[phase], `case ${caseId} ${phase}`);
		if (steps.length > 0) {
			phases[phase] = steps;
		}
	}
	return phases;
}

function normalizeWorkflowSteps(steps) {
	const normalized = normalizeRecipeSteps(steps, 'workflow');
	return normalized.length > 0 ? normalized : [DEFAULT_WORKFLOW_STEP];
}

function normalizeRecipeSteps(steps, label) {
	return normalizeArray(steps).map((step, index) => normalizeRecipeStep(step, `${label} step ${index}`));
}

function normalizeRecipeStep(step = {}, label = 'recipe step') {
	if (!step || typeof step !== 'object' || Array.isArray(step)) {
		throw new Error(`${label} must be an object.`);
	}
	if (typeof step.command !== 'string' || step.command.trim() === '') {
		throw new Error(`${label} requires command.`);
	}

	return stripUndefined({
		command: step.command,
		args: normalizeStepArgs(step.args),
		allowFailure: step.allowFailure ?? step.allow_failure,
		advisory: step.advisory,
		timeoutSeconds: numberOrUndefined(step.timeoutSeconds || step.timeout_seconds),
		metadata: objectOrUndefined(step.metadata),
	});
}

function normalizeStepArgs(args) {
	if (args === undefined || args === null) {
		return undefined;
	}
	if (!Array.isArray(args)) {
		throw new Error('Recipe step args must be an array when provided.');
	}
	return args.map((arg) => String(arg));
}

function normalizeMounts(mounts) {
	return normalizeArray(mounts).map((mount, index) => {
		if (!mount || typeof mount !== 'object' || Array.isArray(mount)) {
			throw new Error(`Recipe mount ${index} must be an object.`);
		}
		if (typeof mount.source !== 'string' || mount.source.trim() === '') {
			throw new Error(`Recipe mount ${index} requires source.`);
		}
		if (typeof mount.target !== 'string' || !mount.target.startsWith('/')) {
			throw new Error(`Recipe mount ${index} requires an absolute target.`);
		}
		return { ...mount, mode: mount.mode || 'readonly' };
	});
}

function normalizeDeclaredArtifacts(artifacts) {
	const normalized = normalizeArray(artifacts).map((artifact, index) => {
		if (!artifact || typeof artifact !== 'object' || Array.isArray(artifact)) {
			throw new Error(`Fuzz case artifact ${index} must be an object.`);
		}
		if (typeof artifact.name !== 'string' || artifact.name.trim() === '') {
			throw new Error(`Fuzz case artifact ${index} requires name.`);
		}
		if (typeof artifact.path !== 'string' || artifact.path.trim() === '') {
			throw new Error(`Fuzz case artifact ${index} requires path.`);
		}
		return stripUndefined({
			name: artifact.name,
			path: artifact.path,
			required: artifact.required,
			metadata: objectOrUndefined(artifact.metadata),
		});
	});
	return normalized.length > 0 ? normalized : undefined;
}

function normalizeArray(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function objectWithEntries(value) {
	const cleaned = stripUndefined(value);
	return Object.keys(cleaned).length > 0 ? cleaned : undefined;
}

function numberOrUndefined(value) {
	if (value === undefined || value === null || value === '') {
		return undefined;
	}
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : undefined;
}

function stripUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(
		Object.entries(value).filter(([, entry]) => entry !== undefined)
	);
}

module.exports = {
	WORDPRESS_FUZZ_PLAN_RECIPE_BUILDER_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	WP_CODEBOX_WORKSPACE_RECIPE_SCHEMA,
	buildWpCodeboxFuzzPlanRecipe,
};
