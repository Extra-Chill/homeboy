'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const AJAX_ACTION_SURFACE_SCHEMA = 'homeboy/wordpress-ajax-action-surface/v1';
const AJAX_ACTION_PLAN_SCHEMA = 'homeboy/wordpress-ajax-action-plan/v1';

const AJAX_HOOK_PREFIXES = Object.freeze({
	anonymous: 'wp_ajax_nopriv_',
	authenticated: 'wp_ajax_',
});

const SAFE_ACTION_TERMS = Object.freeze([
	'check',
	'count',
	'describe',
	'fetch',
	'find',
	'get',
	'list',
	'load',
	'lookup',
	'preview',
	'query',
	'read',
	'refresh',
	'render',
	'search',
	'status',
	'validate',
	'view',
]);

const MUTATING_ACTION_TERMS = Object.freeze([
	'add',
	'apply',
	'approve',
	'cancel',
	'clear',
	'clone',
	'create',
	'delete',
	'disable',
	'dismiss',
	'edit',
	'enable',
	'execute',
	'generate',
	'import',
	'install',
	'migrate',
	'move',
	'publish',
	'purge',
	'remove',
	'repair',
	'replace',
	'reset',
	'restore',
	'retry',
	'run',
	'save',
	'send',
	'set',
	'submit',
	'sync',
	'trash',
	'update',
	'upload',
	'write',
]);

const SENSITIVE_ACTION_TERMS = Object.freeze([
	'auth',
	'backup',
	'credential',
	'export',
	'file',
	'key',
	'login',
	'nonce',
	'password',
	'payment',
	'secret',
	'token',
	'user',
]);

function normalizeWordPressAjaxActionSurface(input = {}, options = {}) {
	const hooks = normalizeAjaxHookInputs(input);
	const byAction = new Map();

	for (const hook of hooks) {
		const parsed = parseAjaxHookName(hook.hook);
		if (!parsed) {
			continue;
		}
		if (!byAction.has(parsed.action)) {
			byAction.set(parsed.action, {
				action: parsed.action,
				hooks: [],
				authenticated: false,
				anonymous: false,
			});
		}
		const entry = byAction.get(parsed.action);
		entry[parsed.audience] = true;
		entry.hooks.push({
			hook: parsed.hook,
			audience: parsed.audience,
			callback: callbackName(hook.callback || hook.function || hook.handler),
			source: sourceSummary(hook),
		});
	}

	const actions = [...byAction.values()]
		.map((entry) => normalizeAjaxAction(entry, options))
		.sort((a, b) => a.action.localeCompare(b.action));

	return {
		schema: AJAX_ACTION_SURFACE_SCHEMA,
		type: 'wordpress-ajax-action-surface',
		actions,
		totals: {
			actionCount: actions.length,
			hookCount: actions.reduce((sum, action) => sum + action.hooks.length, 0),
			authenticatedCount: actions.filter((action) => action.authenticated).length,
			anonymousCount: actions.filter((action) => action.anonymous).length,
			planEligibleCount: actions.filter((action) => action.plan.eligible).length,
			skippedCount: actions.filter((action) => !action.plan.eligible).length,
		},
		metadata: {
			discovery: 'wp_ajax_* and wp_ajax_nopriv_* hook inventory',
			planner: 'homeboy-extension-wordpress/ajax-action-surface',
		},
	};
}

function buildAjaxActionPlanArtifact(input = {}, options = {}) {
	const surface = input.schema === AJAX_ACTION_SURFACE_SCHEMA
		? input
		: normalizeWordPressAjaxActionSurface(input, options);
	const actions = surface.actions.map((action) => normalizeAjaxAction(action, options));
	const plannedActions = actions.filter((action) => action.plan.eligible).map((action) => ({
		id: action.id,
		action: action.action,
		auth: action.authenticated ? 'authenticated' : 'anonymous',
		method: 'POST',
		path: '/wp-admin/admin-ajax.php',
		body: { action: action.action },
		safety: action.safety,
		requiresFixture: true,
		notes: [
			'Plan only: executor must provide nonce, user/session, and action-specific inputs before issuing requests.',
		],
	}));

	return {
		schema: AJAX_ACTION_PLAN_SCHEMA,
		type: 'wordpress-ajax-action-plan',
		actions,
		plannedActions,
		skippedActions: actions.filter((action) => !action.plan.eligible).map((action) => ({
			id: action.id,
			action: action.action,
			safety: action.safety,
			skipReasons: action.plan.skipReasons,
		})),
		totals: {
			actionCount: actions.length,
			plannedCount: plannedActions.length,
			skippedCount: actions.length - plannedActions.length,
		},
		metadata: surface.metadata,
	};
}

function ajaxActionPlanKey(action) {
	const slug = String(action || '')
		.trim()
		.replace(/[^A-Za-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.toLowerCase() || 'unknown';
	return `ajax:${slug}`;
}

function classifyAjaxAction(action, entry = {}) {
	const terms = actionTerms(action);
	const reasons = [];
	let level = 'unknown';
	let intent = 'unknown';

	if (terms.some((term) => MUTATING_ACTION_TERMS.includes(term))) {
		level = 'high';
		intent = 'mutation';
		reasons.push('mutating_action_name');
	} else if (terms.some((term) => SENSITIVE_ACTION_TERMS.includes(term))) {
		level = 'medium';
		intent = 'sensitive';
		reasons.push('sensitive_action_name');
	} else if (terms.some((term) => SAFE_ACTION_TERMS.includes(term))) {
		level = 'low';
		intent = 'read';
		reasons.push('read_like_action_name');
	} else {
		reasons.push('unknown_action_intent');
	}

	if (entry.anonymous) {
		reasons.push('has_nopriv_variant');
		if (level === 'low') {
			level = 'medium';
		}
	}

	return {
		level,
		intent,
		reasons,
	};
}

function ajaxActionSkipReasons(action, safety, options = {}) {
	const reasons = [];
	if (action.anonymous && !options.includeUnauthenticated) {
		reasons.push('unauthenticated_ajax_requires_explicit_opt_in');
	}
	if (safety.intent === 'mutation' && !options.includeMutating) {
		reasons.push('mutating_action_requires_explicit_opt_in');
	}
	if (safety.intent === 'sensitive' && !options.includeSensitive) {
		reasons.push('sensitive_action_requires_explicit_opt_in');
	}
	if (safety.level === 'unknown' && !options.includeUnknown) {
		reasons.push('unknown_safety_requires_manual_review');
	}
	if (action.hooks.length === 0) {
		reasons.push('no_ajax_hook_registered');
	}
	return reasons;
}

function formatAjaxActionPlanMarkdownReport(input = {}, options = {}) {
	const artifact = input.schema === AJAX_ACTION_PLAN_SCHEMA ? input : buildAjaxActionPlanArtifact(input, options);
	const limit = numericOption(options.limit, 20);
	const lines = [
		`## ${options.title || 'WordPress AJAX action surface plan'}`,
		'',
		`Actions: ${artifact.totals.actionCount}; planned: ${artifact.totals.plannedCount}; skipped: ${artifact.totals.skippedCount}`,
		'',
		'## Planned actions',
		'',
		'| Action | Safety | Auth | Hook count |',
		'|---|---:|---|---:|',
	];

	for (const action of artifact.actions.filter((entry) => entry.plan.eligible).slice(0, limit)) {
		lines.push(`| \`${action.action}\` | ${action.safety.level}/${action.safety.intent} | ${action.authenticated ? 'authenticated' : 'anonymous'} | ${action.hooks.length} |`);
	}

	lines.push('', '## Skipped actions', '', '| Action | Safety | Skip reasons |', '|---|---:|---|');
	for (const action of artifact.actions.filter((entry) => !entry.plan.eligible).slice(0, limit)) {
		lines.push(`| \`${action.action}\` | ${action.safety.level}/${action.safety.intent} | ${action.plan.skipReasons.join(', ')} |`);
	}

	return `${lines.join('\n')}\n`;
}

function normalizeAjaxAction(entry = {}, options = {}) {
	const action = String(entry.action || '').trim();
	if (!action) {
		throw new TypeError('AJAX actions require action');
	}
	const hooks = Array.isArray(entry.hooks) ? entry.hooks.map(normalizeAjaxActionHook).filter(Boolean) : [];
	const normalized = {
		id: entry.id || ajaxActionPlanKey(action),
		action,
		authenticated: Boolean(entry.authenticated || hooks.some((hook) => hook.audience === 'authenticated')),
		anonymous: Boolean(entry.anonymous || hooks.some((hook) => hook.audience === 'anonymous')),
		hooks,
	};
	const safety = entry.safety || classifyAjaxAction(action, normalized);
	const skipReasons = shouldPreservePlan(entry, options)
		? entry.plan.skipReasons
		: entry.skipReasons || ajaxActionSkipReasons(normalized, safety, options);
	return {
		...normalized,
		safety,
		plan: {
			eligible: skipReasons.length === 0,
			skipReasons,
		},
	};
}

function normalizeAjaxActionHook(hook) {
	if (!isPlainObject(hook)) {
		return null;
	}
	const parsed = parseAjaxHookName(hook.hook || hook.name);
	return {
		hook: parsed?.hook || String(hook.hook || hook.name || '').trim(),
		audience: hook.audience || parsed?.audience || '',
		callback: callbackName(hook.callback),
		source: sourceSummary(hook),
	};
}

function parseAjaxHookName(value) {
	const hook = String(value || '').trim();
	for (const [audience, prefix] of Object.entries(AJAX_HOOK_PREFIXES)) {
		if (hook.startsWith(prefix) && hook.length > prefix.length) {
			return {
				hook,
				audience,
				action: hook.slice(prefix.length),
			};
		}
	}
	return null;
}

function normalizeAjaxHookInputs(input) {
	if (Array.isArray(input)) {
		return input.flatMap(normalizeAjaxHookInputs);
	}
	if (typeof input === 'string') {
		return [{ hook: input }];
	}
	if (!isPlainObject(input)) {
		return [];
	}
	if (Array.isArray(input.hooks)) {
		return normalizeAjaxHookInputs(input.hooks);
	}
	if (Array.isArray(input.actions)) {
		return input.actions.flatMap((action) => actionToHookInputs(action));
	}
	if (input.hook || input.name) {
		return [{ ...input, hook: input.hook || input.name }];
	}
	return Object.entries(input).flatMap(([hook, value]) => {
		if (!parseAjaxHookName(hook)) {
			return [];
		}
		if (Array.isArray(value)) {
			return value.map((callback) => ({ hook, callback }));
		}
		return [{ ...(isPlainObject(value) ? value : { callback: value }), hook }];
	});
}

function actionToHookInputs(action) {
	if (typeof action === 'string') {
		return [{ hook: `${AJAX_HOOK_PREFIXES.authenticated}${action}` }];
	}
	if (!isPlainObject(action)) {
		return [];
	}
	if (action.hooks) {
		return normalizeAjaxHookInputs(action.hooks);
	}
	const name = String(action.action || action.name || '').trim();
	if (!name) {
		return [];
	}
	const hooks = [];
	if (action.authenticated !== false) {
		hooks.push({ ...action, hook: `${AJAX_HOOK_PREFIXES.authenticated}${name}` });
	}
	if (action.anonymous || action.nopriv) {
		hooks.push({ ...action, hook: `${AJAX_HOOK_PREFIXES.anonymous}${name}` });
	}
	return hooks;
}

function actionTerms(action) {
	return String(action || '')
		.toLowerCase()
		.split(/[^a-z0-9]+/)
		.filter(Boolean);
}

function callbackName(callback) {
	if (Array.isArray(callback)) {
		return callback.map(callbackName).filter(Boolean).join('::');
	}
	if (typeof callback === 'function') {
		return callback.name || '[anonymous function]';
	}
	if (callback === undefined || callback === null) {
		return '';
	}
	return String(callback);
}

function sourceSummary(entry = {}) {
	return entry.source || entry.file || entry.path || '';
}

function numericOption(value, fallback) {
	const number = Number(value);
	return Number.isFinite(number) && number > 0 ? number : fallback;
}

function shouldPreservePlan(entry, options) {
	return Boolean(
		entry.plan?.skipReasons
		&& !options.includeUnauthenticated
		&& !options.includeMutating
		&& !options.includeSensitive
		&& !options.includeUnknown
	);
}

module.exports = {
	AJAX_ACTION_PLAN_SCHEMA,
	AJAX_ACTION_SURFACE_SCHEMA,
	ajaxActionPlanKey,
	buildAjaxActionPlanArtifact,
	classifyAjaxAction,
	formatAjaxActionPlanMarkdownReport,
	normalizeWordPressAjaxActionSurface,
	parseAjaxHookName,
};
