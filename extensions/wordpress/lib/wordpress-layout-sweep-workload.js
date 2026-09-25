'use strict';

/**
 * External dependencies
 */
const crypto = require('node:crypto');
const fs = require('node:fs/promises');
const path = require('node:path');

/**
 * Internal dependencies
 */
const { runWpCodeboxRecipe } = require('./wp-codebox-recipe-helper');
const { resolveWpCodeboxArtifactPath } = require('./wp-codebox-artifacts');

// The `wordpress.layout-sweep` command contract this module builds requests
// against and maps results from. It is documented in Automattic/wp-codebox#2530
// (not yet merged as of this writing) and is kept in sync with the README
// section it adds: inputs, scenarios, finding kinds, the
// `wp-codebox/layout-sweep/v1` summary, and the
// `files/browser/layout-sweep/{summary,findings}.json` artifacts.
const LAYOUT_SWEEP_COMMAND = 'wordpress.layout-sweep';
const LAYOUT_SWEEP_SUMMARY_SCHEMA = 'wp-codebox/layout-sweep/v1';
const LAYOUT_SWEEP_ARTIFACT_PREFIX = 'files/browser/layout-sweep';
const LAYOUT_SWEEP_WORKLOAD_SCHEMA = 'homeboy/wordpress-layout-sweep-workload/v1';
const LAYOUT_SWEEP_FINDING_SCHEMA = 'homeboy/fuzz-finding/v1';
const LAYOUT_SWEEP_OBSERVATION_SCHEMA = 'homeboy/fuzz-observation/v1';
const HOMEBOY_FUZZ_CAMPAIGN_SCHEMA = 'homeboy/fuzz-campaign/v1';

const DEFAULT_MIN_WIDTH = 320;
const DEFAULT_MAX_WIDTH = 1920;
const DEFAULT_PROFILE = 'quick';
const DEFAULT_SEED = 1;
const DEFAULT_SCENARIOS = Object.freeze(['sweep', 'history', 'storm', 'heights']);
const CORE_SCENARIO_NAMES = new Set(['sweep', 'history', 'storm', 'heights', 'drag']);

const TOP_LEVEL_DECLARATION_FIELDS = new Set([
	'schema', 'id', 'label', 'preview', 'containerSelector', 'itemSelector', 'modeProperty',
	'minWidth', 'maxWidth', 'profile', 'seed', 'concurrency', 'scenarios',
	'accepted', 'timeout', 'metadata',
]);
const PREVIEW_FIELDS = new Set(['url', 'recipe']);
const ACCEPTED_FIELDS = new Set(['kind', 'container', 'item']);

/**
 * Validate and normalize a layout-sweep workload declaration.
 *
 * The declaration is the product-specific surface a component owns: a
 * preview target (a URL, or a Codebox recipe that boots a WordPress build
 * with the component under test), container and item selectors, a width
 * range, a profile, a seed, scenarios, and accepted findings. Unknown fields
 * are rejected so declarations fail loudly instead of silently ignoring a
 * mistyped option.
 * @param {Object} declaration Raw layout-sweep workload declaration.
 * @return {Object} Normalized declaration.
 */
function normalizeLayoutSweepWorkloadDeclaration(declaration) {
	assertPlainObject(declaration, 'layout-sweep workload declaration');
	rejectUnknownFields(declaration, TOP_LEVEL_DECLARATION_FIELDS, 'layout-sweep workload declaration');
	if (declaration.schema !== undefined && declaration.schema !== LAYOUT_SWEEP_WORKLOAD_SCHEMA) {
		throw new Error(`Unsupported layout-sweep workload declaration schema: ${declaration.schema}`);
	}

	const preview = normalizeLayoutSweepPreview(declaration.preview);
	const containerSelector = requiredNonEmptyString(declaration.containerSelector, 'containerSelector');
	const itemSelector = requiredNonEmptyString(declaration.itemSelector, 'itemSelector');
	if (containerSelector.length > 512 || itemSelector.length > 512) {
		throw new Error('layout-sweep workload declaration selectors must be at most 512 characters.');
	}

	// Container custom property naming the current layout mode, so breakpoint
	// switches separate bands instead of reading as jumps or new overlaps.
	let modeProperty;
	if (declaration.modeProperty !== undefined) {
		modeProperty = requiredNonEmptyString(declaration.modeProperty, 'modeProperty');
		if (!/^--[A-Za-z0-9_-]{1,120}$/.test(modeProperty)) {
			throw new Error('layout-sweep workload declaration modeProperty must be a CSS custom property name such as --layout-mode.');
		}
	}

	const minWidth = positiveIntegerOrDefault(declaration.minWidth, DEFAULT_MIN_WIDTH, 'minWidth');
	const maxWidth = positiveIntegerOrDefault(declaration.maxWidth, DEFAULT_MAX_WIDTH, 'maxWidth');
	if (minWidth > maxWidth) {
		throw new Error('layout-sweep workload declaration minWidth must not exceed maxWidth.');
	}

	const profile = declaration.profile === undefined
		? DEFAULT_PROFILE
		: requireEnum(declaration.profile, ['quick', 'deep'], 'profile');

	const seed = positiveIntegerOrDefault(declaration.seed, DEFAULT_SEED, 'seed');

	let concurrency;
	if (declaration.concurrency !== undefined) {
		concurrency = positiveIntegerOrDefault(declaration.concurrency, undefined, 'concurrency');
		if (concurrency < 1 || concurrency > 8) {
			throw new Error('layout-sweep workload declaration concurrency must be from 1 to 8.');
		}
	}

	const scenarios = normalizeLayoutSweepScenarios(declaration.scenarios);
	const accepted = normalizeLayoutSweepAccepted(declaration.accepted);
	const timeout = declaration.timeout === undefined ? undefined : declaration.timeout;

	return stripUndefined({
		schema: LAYOUT_SWEEP_WORKLOAD_SCHEMA,
		id: declaration.id ? String(declaration.id) : 'layout-sweep',
		label: declaration.label ? String(declaration.label) : undefined,
		preview,
		containerSelector,
		itemSelector,
		modeProperty,
		minWidth,
		maxWidth,
		profile,
		seed,
		concurrency,
		scenarios,
		accepted,
		timeout,
		metadata: objectOrUndefined(declaration.metadata) || {},
	});
}

function normalizeLayoutSweepPreview(preview) {
	assertPlainObject(preview, 'layout-sweep workload declaration.preview');
	rejectUnknownFields(preview, PREVIEW_FIELDS, 'layout-sweep workload declaration.preview');
	const url = requiredNonEmptyString(preview.url, 'preview.url');
	const recipe = preview.recipe === undefined ? undefined : assertPlainObject(preview.recipe, 'preview.recipe');
	return stripUndefined({ url, recipe });
}

function normalizeLayoutSweepScenarios(scenarios) {
	if (scenarios === undefined) {
		return [...DEFAULT_SCENARIOS];
	}
	if (!Array.isArray(scenarios) || scenarios.length === 0) {
		throw new Error('layout-sweep workload declaration scenarios must be a non-empty array of scenario tokens.');
	}
	return scenarios.map((token) => {
		if (typeof token !== 'string' || token.trim() === '') {
			throw new Error('layout-sweep workload declaration scenarios must be non-empty strings.');
		}
		const trimmed = token.trim();
		assertValidLayoutSweepScenarioToken(trimmed);
		return trimmed;
	});
}

function assertValidLayoutSweepScenarioToken(token) {
	if (CORE_SCENARIO_NAMES.has(token)) {
		return;
	}
	if (/^text-scale:\d+(\.\d+)?$/.test(token)) {
		return;
	}
	if (token === 'block-fonts') {
		return;
	}
	if (/^long-text:\d+(\.\d+)?$/.test(token)) {
		return;
	}
	if (/^dpr:\d+(\.\d+)?$/.test(token)) {
		return;
	}
	throw new Error(`layout-sweep workload declaration scenarios must be ${[...CORE_SCENARIO_NAMES].join(', ')}, text-scale:<percent>, block-fonts, long-text:<ratio>, or dpr:<n>: ${token}`);
}

function normalizeLayoutSweepAccepted(accepted) {
	if (accepted === undefined) {
		return [];
	}
	if (!Array.isArray(accepted)) {
		throw new Error('layout-sweep workload declaration accepted must be an array of {kind, container, item}.');
	}
	return accepted.map((entry, index) => {
		assertPlainObject(entry, `accepted[${index}]`);
		rejectUnknownFields(entry, ACCEPTED_FIELDS, `accepted[${index}]`);
		const kind = requiredNonEmptyString(entry.kind, `accepted[${index}].kind`);
		return {
			kind,
			container: entry.container === undefined || entry.container === null ? null : String(entry.container),
			item: entry.item === undefined || entry.item === null ? null : String(entry.item),
		};
	});
}

/**
 * Build the `wordpress.layout-sweep` Codebox command args for a declaration.
 *
 * This is the single adapter that knows the `wordpress.layout-sweep`
 * argument contract; when that contract changes upstream, only this function
 * needs to change.
 * @param {Object} declaration Layout-sweep workload declaration (raw or normalized).
 * @return {string[]} `key=value` command args for `wordpress.layout-sweep`.
 */
function buildLayoutSweepCodeboxArgs(declaration) {
	const normalized = normalizeLayoutSweepWorkloadDeclaration(declaration);
	const args = [
		`url=${normalized.preview.url}`,
		`container-selector=${normalized.containerSelector}`,
		`item-selector=${normalized.itemSelector}`,
	];
	if (normalized.modeProperty !== undefined) {
		args.push(`mode-property=${normalized.modeProperty}`);
	}
	args.push(
		`min-width=${normalized.minWidth}`,
		`max-width=${normalized.maxWidth}`,
		`profile=${normalized.profile}`,
		`seed=${normalized.seed}`,
	);
	if (normalized.concurrency !== undefined) {
		args.push(`concurrency=${normalized.concurrency}`);
	}
	args.push(`scenarios=${normalized.scenarios.join(',')}`);
	if (normalized.accepted.length > 0) {
		args.push(`accepted=${JSON.stringify(normalized.accepted)}`);
	}
	if (normalized.timeout !== undefined) {
		args.push(`timeout=${normalized.timeout}`);
	}
	return args;
}

/**
 * Build the `wordpress.layout-sweep` workflow step for a WP Codebox recipe.
 * @param {Object} declaration Layout-sweep workload declaration (raw or normalized).
 * @return {{command: string, args: string[]}} Recipe workflow step.
 */
function buildLayoutSweepRecipeStep(declaration) {
	return {
		command: LAYOUT_SWEEP_COMMAND,
		args: buildLayoutSweepCodeboxArgs(declaration),
	};
}

/**
 * Build a full WP Codebox recipe that runs a layout-sweep declaration.
 *
 * When `declaration.preview.recipe` is present (for example, a recipe that
 * mounts a component build and demo content), its setup steps run before the
 * `wordpress.layout-sweep` step so `declaration.preview.url` resolves inside
 * the booted candidate.
 * @param {Object} declaration       Layout-sweep workload declaration (raw or normalized).
 * @param {Object} options           Recipe options.
 * @param {string} options.artifactDirectory Directory WP Codebox should write artifacts to.
 * @return {Object} `wp-codebox/workspace-recipe/v1` recipe.
 */
function buildLayoutSweepRecipe(declaration, options = {}) {
	const normalized = normalizeLayoutSweepWorkloadDeclaration(declaration);
	const artifactDirectory = requiredNonEmptyString(options.artifactDirectory, 'options.artifactDirectory');
	const step = buildLayoutSweepRecipeStep(normalized);
	const base = {
		schema: 'wp-codebox/workspace-recipe/v1',
		workflow: {},
		artifacts: { directory: artifactDirectory },
	};
	const recipe = deepMerge(base, normalized.preview.recipe || {});
	const setupSteps = Array.isArray(normalized.preview.recipe?.workflow?.steps) ? normalized.preview.recipe.workflow.steps : [];
	return {
		...recipe,
		workflow: {
			...(recipe.workflow || {}),
			steps: [...setupSteps, step],
		},
		artifacts: {
			...(recipe.artifacts || {}),
			directory: artifactDirectory,
		},
	};
}

/**
 * Compute a stable fingerprint for a layout-sweep finding identity.
 *
 * Identity is kind + container + item, matching how `wordpress.layout-sweep`
 * itself groups findings. The fingerprint is what makes two campaigns from
 * the same workload comparable: the same container/item regression keeps the
 * same fingerprint across runs even when unrelated findings appear or
 * disappear.
 * @param {{kind: string, container: (string|null), item: (string|null)}} identity Finding identity.
 * @return {string} Stable hex fingerprint.
 */
function layoutSweepFindingFingerprint(identity) {
	const key = JSON.stringify([identity.kind, identity.container ?? null, identity.item ?? null]);
	return crypto.createHash('sha1').update(key).digest('hex');
}

/**
 * Map a `wp-codebox/layout-sweep/v1` summary's grouped findings to
 * `homeboy/fuzz-finding/v1` entries.
 *
 * Identity is kind + container + item. Evidence is width range, worst
 * magnitude, scenarios, count, and sample. Replay metadata comes from seed,
 * profile, and args. Accepted groups (`suppressed: true` in the summary) map
 * to status `suppressed`. `error` findings are mapped like any other finding
 * kind; only the `drag` perf block is diverted to report-only observations
 * (see `mapLayoutSweepPerfToObservations`).
 * @param {Object} summary          `wp-codebox/layout-sweep/v1` summary.
 * @param {Object} [options]        Mapping options.
 * @param {string} [options.workloadId] Workload id recorded on finding metadata.
 * @return {Object[]} `homeboy/fuzz-finding/v1` entries.
 */
function mapLayoutSweepSummaryToFuzzFindings(summary, options = {}) {
	assertPlainObject(summary, 'layout-sweep summary');
	if (summary.schema !== undefined && summary.schema !== LAYOUT_SWEEP_SUMMARY_SCHEMA) {
		throw new Error(`Unsupported layout-sweep summary schema: ${summary.schema}`);
	}
	const groups = asArray(summary.findings, 'summary.findings');
	const summaryReplayArgs = asArray(summary.replay?.args, 'summary.replay.args');

	return groups.map((group, index) => {
		assertPlainObject(group, `summary.findings[${index}]`);
		const kind = requiredNonEmptyString(group.kind, `summary.findings[${index}].kind`);
		const identity = {
			kind,
			container: group.container ?? null,
			item: group.item ?? null,
		};
		const fingerprint = layoutSweepFindingFingerprint(identity);
		const replayArgs = Array.isArray(group.replay?.args) ? group.replay.args : summaryReplayArgs;

		const where = [identity.container, identity.item].filter(Boolean).join(' / ');
		return {
			schema: LAYOUT_SWEEP_FINDING_SCHEMA,
			id: fingerprint,
			// Homeboy fuzz findings require a title and severity. Layout findings
			// use the extension's shared `error` severity; policy comes from status.
			title: where ? `Layout ${kind}: ${where}` : `Layout ${kind}`,
			severity: 'error',
			fingerprint,
			workload_id: options.workloadId,
			kind,
			identity,
			status: group.suppressed === true ? 'suppressed' : 'open',
			evidence: {
				width_range: Array.isArray(group.widthRange) ? group.widthRange : null,
				worst_magnitude: Number.isFinite(group.worstMagnitude) ? group.worstMagnitude : null,
				scenarios: Array.isArray(group.scenarios) ? [...group.scenarios] : [],
				count: Number.isFinite(group.count) ? group.count : 0,
				sample: group.sample ?? null,
			},
			replay: {
				command: LAYOUT_SWEEP_COMMAND,
				seed: summary.seed ?? null,
				profile: summary.profile ?? null,
				args: replayArgs,
			},
			metadata: stripUndefined({
				workload_id: options.workloadId,
				source: LAYOUT_SWEEP_COMMAND,
			}),
		};
	});
}

/**
 * Map a `wp-codebox/layout-sweep/v1` summary's `drag` perf block to
 * report-only observations.
 *
 * The drag cost probe is a performance signal, not a layout regression: it
 * never gates findings. This mapper keeps it structurally separate from
 * `mapLayoutSweepSummaryToFuzzFindings` so downstream consumers cannot
 * accidentally treat it as a pass/fail finding.
 * @param {Object} summary   `wp-codebox/layout-sweep/v1` summary.
 * @param {Object} [options] Mapping options.
 * @return {Object[]} Zero or one `homeboy/fuzz-observation/v1` entries.
 */
function mapLayoutSweepPerfToObservations(summary, options = {}) {
	assertPlainObject(summary, 'layout-sweep summary');
	if (!objectOrUndefined(summary.perf)) {
		return [];
	}
	const perf = summary.perf;
	return [{
		schema: LAYOUT_SWEEP_OBSERVATION_SCHEMA,
		kind: 'drag',
		report_only: true,
		gates: false,
		status: perf.status || 'unavailable',
		metrics: stripUndefined({
			steps: perf.steps,
			ms_per_step: perf.msPerStep,
			layout_ms_per_step: perf.layoutMsPerStep,
			style_ms_per_step: perf.styleMsPerStep,
			script_ms_per_step: perf.scriptMsPerStep,
			layouts_per_step: perf.layoutsPerStep,
			long_tasks: perf.longTasks,
			longest_task_ms: perf.longestTaskMs,
		}),
		message: perf.message,
		replay: {
			command: LAYOUT_SWEEP_COMMAND,
			seed: summary.seed ?? null,
			profile: summary.profile ?? null,
			args: asArray(summary.replay?.args, 'summary.replay.args'),
		},
		metadata: stripUndefined({
			workload_id: options.workloadId,
			source: LAYOUT_SWEEP_COMMAND,
		}),
	}];
}

/**
 * Map a layout-sweep summary to a full workload result: findings,
 * report-only observations, and roll-up metrics.
 * @param {Object} summary   `wp-codebox/layout-sweep/v1` summary.
 * @param {Object} [options] Mapping options (forwarded to the finding/observation mappers).
 * @return {{metrics: Object, findings: Object[], observations: Object[], metadata: Object}} Workload result.
 */
function mapLayoutSweepSummaryToWorkloadResult(summary, options = {}) {
	const findings = mapLayoutSweepSummaryToFuzzFindings(summary, options);
	const observations = mapLayoutSweepPerfToObservations(summary, options);
	const openFindings = findings.filter((finding) => finding.status !== 'suppressed');
	return {
		metrics: {
			layout_sweep_pass: openFindings.length === 0 ? 1 : 0,
			layout_sweep_finding_count: findings.length,
			layout_sweep_open_finding_count: openFindings.length,
			layout_sweep_suppressed_finding_count: findings.length - openFindings.length,
		},
		findings,
		observations,
		metadata: stripUndefined({
			layout_sweep_schema: summary.schema,
			layout_sweep_status: summary.status,
			seed: summary.seed,
			profile: summary.profile,
			workload_id: options.workloadId,
		}),
	};
}

/**
 * Wrap a mapped layout-sweep result in a minimal `homeboy/fuzz-campaign/v1`
 * envelope so `homeboy fuzz compare` can operate on two campaigns from the
 * same workload.
 * @param {Object} options             Campaign options.
 * @param {string} [options.id]        Campaign id. Defaults to the declaration id.
 * @param {Object} options.declaration Layout-sweep workload declaration (raw or normalized).
 * @param {Object} options.summary     `wp-codebox/layout-sweep/v1` summary.
 * @param {string} [options.safetyClass] Homeboy fuzz safety class. Defaults to `read_only`.
 * @return {Object} `homeboy/fuzz-campaign/v1` campaign.
 */
function buildLayoutSweepFuzzCampaign(options = {}) {
	assertPlainObject(options, 'buildLayoutSweepFuzzCampaign options');
	const normalizedDeclaration = normalizeLayoutSweepWorkloadDeclaration(options.declaration);
	const id = options.id ? String(options.id) : normalizedDeclaration.id;
	const result = mapLayoutSweepSummaryToWorkloadResult(options.summary, { workloadId: id });
	return {
		schema: HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
		version: 1,
		id,
		title: `WordPress layout-sweep campaign ${id}`,
		safety_class: options.safetyClass || 'read_only',
		findings: result.findings,
		observations: result.observations,
		metadata: {
			...result.metadata,
			declaration: normalizedDeclaration,
		},
	};
}

/**
 * Run a layout-sweep workload declaration end to end: build the recipe, run
 * it through WP Codebox, read the `wordpress.layout-sweep` artifacts, and map
 * the result. This is the only function in this module that invokes
 * WP Codebox; everything upstream of it is pure and unit-testable against
 * fixture summaries.
 * @param {Object} options                    Run options.
 * @param {Object} options.declaration        Layout-sweep workload declaration (raw or normalized).
 * @param {string} options.artifactsDirectory Directory WP Codebox should write artifacts to.
 * @param {string} [options.recipeFile]       Where to write the generated recipe. Defaults inside artifactsDirectory.
 * @param {string} [options.outputFile]       Where WP Codebox should write its JSON result.
 * @param {string} [options.wpCodeboxBin]     WP Codebox binary override.
 * @return {Promise<Object>} Workload result plus the raw Codebox result, recipe, and summary.
 */
async function runWordPressLayoutSweepWorkload(options = {}) {
	assertPlainObject(options, 'runWordPressLayoutSweepWorkload options');
	const declaration = normalizeLayoutSweepWorkloadDeclaration(options.declaration || options);
	const artifactsDirectory = requiredNonEmptyString(options.artifactsDirectory, 'options.artifactsDirectory');
	const recipe = buildLayoutSweepRecipe(declaration, { artifactDirectory: artifactsDirectory });
	const recipeFile = options.recipeFile || path.join(artifactsDirectory, 'wp-codebox-layout-sweep-recipe.json');

	await fs.mkdir(path.dirname(recipeFile), { recursive: true });
	await fs.writeFile(recipeFile, `${JSON.stringify(recipe, null, 2)}\n`);

	const run = await runWpCodeboxRecipe({
		recipeFile,
		artifactsDir: artifactsDirectory,
		outputFile: options.outputFile,
		wpCodeboxBin: options.wpCodeboxBin,
		bin: options.bin,
		env: options.env,
		cwd: options.cwd,
		recipeRunArgs: options.recipeRunArgs,
	});

	const codeboxResult = run.json;
	const summaryPath = resolveWpCodeboxArtifactPath({
		codeboxResult,
		artifactsDirectory,
		key: 'layoutSweepSummary',
		fallbackPath: `${LAYOUT_SWEEP_ARTIFACT_PREFIX}/summary.json`,
	});
	const summary = JSON.parse(await fs.readFile(summaryPath, 'utf8'));
	const result = mapLayoutSweepSummaryToWorkloadResult(summary, { workloadId: declaration.id });

	return {
		...result,
		declaration,
		codeboxResult,
		recipe,
		recipeFile,
		summary,
		summaryPath,
	};
}

// -- Small local helpers (mirrors the style of the other WordPress fuzz/wp-codebox lib files) --

function assertPlainObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
	return value;
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

function rejectUnknownFields(object, allowedFields, label) {
	const unknown = Object.keys(object).filter((key) => !allowedFields.has(key));
	if (unknown.length > 0) {
		throw new Error(`${label} has unsupported field(s): ${unknown.join(', ')}. Supported fields: ${[...allowedFields].join(', ')}.`);
	}
}

function requiredNonEmptyString(value, field) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new Error(`${field} must be a non-empty string.`);
	}
	return value.trim();
}

function requireEnum(value, allowed, field) {
	if (!allowed.includes(value)) {
		throw new Error(`${field} must be one of ${allowed.join(', ')}: ${value}`);
	}
	return value;
}

function positiveIntegerOrDefault(value, fallback, field) {
	if (value === undefined) {
		return fallback;
	}
	const parsed = Number(value);
	if (!Number.isInteger(parsed) || parsed <= 0) {
		throw new Error(`${field} must be a positive integer.`);
	}
	return parsed;
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function stripUndefined(value) {
	if (!objectOrUndefined(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function deepMerge(base, override) {
	if (!objectOrUndefined(override)) {
		return base;
	}
	const merged = { ...base };
	for (const [key, value] of Object.entries(override)) {
		if (objectOrUndefined(value) && objectOrUndefined(base[key])) {
			merged[key] = deepMerge(base[key], value);
		} else {
			merged[key] = value;
		}
	}
	return merged;
}

module.exports = {
	LAYOUT_SWEEP_COMMAND,
	LAYOUT_SWEEP_SUMMARY_SCHEMA,
	LAYOUT_SWEEP_ARTIFACT_PREFIX,
	LAYOUT_SWEEP_WORKLOAD_SCHEMA,
	LAYOUT_SWEEP_FINDING_SCHEMA,
	LAYOUT_SWEEP_OBSERVATION_SCHEMA,
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	normalizeLayoutSweepWorkloadDeclaration,
	buildLayoutSweepCodeboxArgs,
	buildLayoutSweepRecipeStep,
	buildLayoutSweepRecipe,
	layoutSweepFindingFingerprint,
	mapLayoutSweepSummaryToFuzzFindings,
	mapLayoutSweepPerfToObservations,
	mapLayoutSweepSummaryToWorkloadResult,
	buildLayoutSweepFuzzCampaign,
	runWordPressLayoutSweepWorkload,
};
