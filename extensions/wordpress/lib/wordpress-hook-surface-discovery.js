'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const WORDPRESS_HOOK_SURFACE_DISCOVERY_SCHEMA = 'homeboy/wordpress-hook-surface-discovery/v1';
const WORDPRESS_HOOK_FUZZ_PLAN_SCHEMA = 'homeboy/wordpress-hook-fuzz-plan/v1';
const HOOK_REGISTRATION_FUNCTIONS = new Set(['add_action', 'add_filter']);
const CRON_SCHEDULING_FUNCTIONS = new Set(['wp_schedule_event', 'wp_schedule_single_event']);

function discoverWordPressHookSurfaces(input = {}) {
	const files = normalizeSourceFiles(input.files || input.sources || input.source || input);
	const options = normalizeOptions(input.options || input);
	const surfaces = [];
	const skipped = [];

	for (const file of files) {
		for (const call of extractFunctionCalls(file.content)) {
			const surface = surfaceFromCall(call, file, options);
			if (!surface) {
				continue;
			}
			if (surface.skipped) {
				skipped.push(surface);
			} else {
				surfaces.push(surface);
			}
		}
	}

	return {
		schema: WORDPRESS_HOOK_SURFACE_DISCOVERY_SCHEMA,
		summary: {
			file_count: files.length,
			surface_count: surfaces.length,
			skipped_count: skipped.length,
		},
		surfaces: sortSurfaces(dedupeSurfaces(surfaces)),
		skipped: options.includeSkipped ? sortSurfaces(dedupeSurfaces(skipped)) : [],
	};
}

function createWordPressHookFuzzPlan(input = {}) {
	const discovery = input.discovery || discoverWordPressHookSurfaces(input);
	const surfaces = Array.isArray(input.surfaces) ? input.surfaces : discovery.surfaces || [];
	const skipped = [
		...(Array.isArray(discovery.skipped) ? discovery.skipped : []),
		...surfaces.filter((surface) => !surface?.invocation?.safe_to_auto_invoke).map((surface) => skippedFromSurface(surface)),
	];
	const cases = surfaces
		.filter((surface) => surface?.invocation?.safe_to_auto_invoke)
		.map((surface) => ({
			id: `hook-fuzz:${surface.id}`,
			surface_id: surface.id,
			hook: surface.hook,
			type: surface.type,
			kind: surface.kind,
			invocation: surface.invocation,
			source: surface.source,
		}));

	return {
		schema: WORDPRESS_HOOK_FUZZ_PLAN_SCHEMA,
		summary: {
			surface_count: surfaces.length,
			case_count: cases.length,
			skipped_count: skipped.length,
		},
		cases,
		skipped: input.includeSkipped === false ? [] : sortSurfaces(dedupeSurfaces(skipped)),
	};
}

function surfaceFromCall(call, file, options) {
	if (HOOK_REGISTRATION_FUNCTIONS.has(call.name)) {
		return hookRegistrationSurface(call, file, options);
	}
	if (CRON_SCHEDULING_FUNCTIONS.has(call.name)) {
		return cronSchedulingSurface(call, file);
	}
	return null;
}

function hookRegistrationSurface(call, file, options) {
	const args = splitTopLevelArgs(call.args);
	const hookName = literalString(args[0]);
	const type = call.name === 'add_filter' ? 'filter' : 'action';

	if (!hookName) {
		return skippedSurface({
			kind: 'hook_registration',
			type,
			call,
			file,
			reason: 'dynamic_hook_name',
			detail: 'Hook registration uses a non-literal hook name.',
		});
	}

	const acceptedArgs = integerLiteral(args[3], 1);
	const priority = integerLiteral(args[2], 10);
	const base = baseSurface({
		kind: 'hook_registration',
		type,
		hook: hookName,
		call,
		file,
		metadata: {
			priority,
			accepted_args: acceptedArgs,
			callback: callbackLabel(args[1]),
		},
	});

	return {
		...base,
		invocation: hookInvocationMetadata({
			type,
			hook: hookName,
			acceptedArgs,
			allowZeroArgFilters: options.allowZeroArgFilters,
		}),
	};
}

function cronSchedulingSurface(call, file) {
	const args = splitTopLevelArgs(call.args);
	const hookName = literalString(call.name === 'wp_schedule_single_event' ? args[1] : args[2]);

	if (!hookName) {
		return skippedSurface({
			kind: 'cron_schedule',
			type: 'cron',
			call,
			file,
			reason: 'dynamic_cron_hook_name',
			detail: 'Cron scheduling call uses a non-literal hook name.',
		});
	}

	return {
		...baseSurface({
			kind: 'cron_schedule',
			type: 'cron',
			hook: hookName,
			call,
			file,
			metadata: {
				recurrence: call.name === 'wp_schedule_event' ? literalString(args[1]) : 'single',
			},
		}),
		invocation: {
			mode: 'wp_cron_event',
			hook: hookName,
			args: [],
			safe_to_auto_invoke: false,
			skip_reason: 'cron_event_requires_runtime_schedule',
			skip_detail: 'Cron events are discovered for planning but require a runtime-scheduled event envelope before invocation.',
			side_effect_risk: 'unknown',
		},
	};
}

function hookInvocationMetadata({ type, hook, acceptedArgs, allowZeroArgFilters }) {
	if (acceptedArgs > 0) {
		return {
			mode: type === 'filter' ? 'apply_filters' : 'do_action',
			hook,
			args: [],
			safe_to_auto_invoke: false,
			skip_reason: 'requires_arguments',
			skip_detail: `Registered callback accepts ${acceptedArgs} argument(s); fuzz planning needs explicit argument fixtures.`,
			side_effect_risk: 'unknown',
		};
	}

	if (type === 'filter' && !allowZeroArgFilters) {
		return {
			mode: 'apply_filters',
			hook,
			args: [],
			safe_to_auto_invoke: false,
			skip_reason: 'filter_requires_seed_value',
			skip_detail: 'Filter invocation needs an explicit seed value even when callbacks declare zero accepted args.',
			side_effect_risk: 'unknown',
		};
	}

	return {
		mode: type === 'filter' ? 'apply_filters' : 'do_action',
		hook,
		args: [],
		safe_to_auto_invoke: true,
		side_effect_risk: 'unknown',
	};
}

function skippedFromSurface(surface = {}) {
	return {
		...surface,
		skipped: true,
		skip_reason: surface.invocation?.skip_reason || 'unsafe_invocation_metadata',
		skip_detail: surface.invocation?.skip_detail || 'Surface lacks safe automatic invocation metadata.',
	};
}

function baseSurface({ kind, type, hook, call, file, metadata = {} }) {
	return {
		id: surfaceId(type, hook, file.path, call.line),
		kind,
		type,
		hook,
		source: {
			path: file.path,
			line: call.line,
			function: call.name,
		},
		metadata: stripUndefined(metadata),
	};
}

function skippedSurface({ kind, type, call, file, reason, detail }) {
	return {
		id: surfaceId(type, `${call.name}:dynamic`, file.path, call.line),
		kind,
		type,
		hook: '',
		skipped: true,
		skip_reason: reason,
		skip_detail: detail,
		source: {
			path: file.path,
			line: call.line,
			function: call.name,
		},
	};
}

function extractFunctionCalls(source) {
	const calls = [];
	const pattern = /\b(add_action|add_filter|wp_schedule_event|wp_schedule_single_event)\s*\(/g;
	let match;
	while ((match = pattern.exec(source)) !== null) {
		const argsStart = pattern.lastIndex;
		const argsEnd = findClosingParen(source, argsStart - 1);
		if (argsEnd === -1) {
			continue;
		}
		calls.push({
			name: match[1],
			args: source.slice(argsStart, argsEnd),
			line: lineNumberAt(source, match.index),
		});
		pattern.lastIndex = argsEnd + 1;
	}
	return calls;
}

function findClosingParen(source, openIndex) {
	let depth = 0;
	let quote = '';
	let escaped = false;
	for (let index = openIndex; index < source.length; index += 1) {
		const char = source[index];
		if (quote) {
			if (escaped) {
				escaped = false;
			} else if (char === '\\') {
				escaped = true;
			} else if (char === quote) {
				quote = '';
			}
			continue;
		}
		if (char === '\'' || char === '"') {
			quote = char;
			continue;
		}
		if (char === '(') {
			depth += 1;
		} else if (char === ')') {
			depth -= 1;
			if (depth === 0) {
				return index;
			}
		}
	}
	return -1;
}

function splitTopLevelArgs(argsSource) {
	const args = [];
	let start = 0;
	let depth = 0;
	let quote = '';
	let escaped = false;
	for (let index = 0; index < argsSource.length; index += 1) {
		const char = argsSource[index];
		if (quote) {
			if (escaped) {
				escaped = false;
			} else if (char === '\\') {
				escaped = true;
			} else if (char === quote) {
				quote = '';
			}
			continue;
		}
		if (char === '\'' || char === '"') {
			quote = char;
			continue;
		}
		if (char === '(' || char === '[' || char === '{') {
			depth += 1;
		} else if (char === ')' || char === ']' || char === '}') {
			depth = Math.max(0, depth - 1);
		} else if (char === ',' && depth === 0) {
			args.push(argsSource.slice(start, index).trim());
			start = index + 1;
		}
	}
	const tail = argsSource.slice(start).trim();
	if (tail) {
		args.push(tail);
	}
	return args;
}

function literalString(value) {
	const raw = String(value || '').trim();
	const match = raw.match(/^(['"])((?:\\.|(?!\1).)*)\1$/s);
	if (!match) {
		return '';
	}
	return match[2].replace(/\\(['"\\])/g, '$1');
}

function integerLiteral(value, fallback) {
	const raw = String(value || '').trim();
	if (!raw) {
		return fallback;
	}
	const parsed = Number(raw);
	return Number.isInteger(parsed) ? parsed : fallback;
}

function callbackLabel(value) {
	const raw = String(value || '').trim();
	if (!raw) {
		return undefined;
	}
	return literalString(raw) || raw.replace(/\s+/g, ' ');
}

function normalizeSourceFiles(value) {
	if (typeof value === 'string') {
		return [{ path: 'inline.php', content: value }];
	}
	if (Array.isArray(value)) {
		return value.flatMap((entry, index) => normalizeSourceFile(entry, index));
	}
	if (isPlainObject(value) && typeof value.content === 'string') {
		return [normalizeSourceFile(value, 0)];
	}
	if (isPlainObject(value)) {
		return Object.entries(value).map(([filePath, content]) => ({
			path: filePath,
			content: String(content || ''),
		}));
	}
	return [];
}

function normalizeSourceFile(entry, index) {
	if (typeof entry === 'string') {
		return [{ path: `inline-${index + 1}.php`, content: entry }];
	}
	if (!isPlainObject(entry)) {
		return [];
	}
	return [{
		path: String(entry.path || entry.file || entry.filename || `inline-${index + 1}.php`),
		content: String(entry.content || entry.source || ''),
	}];
}

function normalizeOptions(options = {}) {
	return {
		allowZeroArgFilters: options.allowZeroArgFilters === true || options.allow_zero_arg_filters === true,
		includeSkipped: options.includeSkipped !== false && options.include_skipped !== false,
	};
}

function lineNumberAt(source, offset) {
	return source.slice(0, offset).split('\n').length;
}

function surfaceId(type, hook, filePath, line) {
	const hookPart = slugify(hook) || 'dynamic';
	const pathPart = slugify(filePath) || 'inline';
	return `${type}:${hookPart}:${pathPart}:${line}`;
}

function slugify(value) {
	return String(value || '')
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.slice(0, 80);
}

function dedupeSurfaces(surfaces) {
	const byId = new Map();
	for (const surface of surfaces) {
		byId.set(surface.id, surface);
	}
	return [...byId.values()];
}

function sortSurfaces(surfaces) {
	return [...surfaces].sort((a, b) => {
		const pathCompare = String(a.source?.path || '').localeCompare(String(b.source?.path || ''));
		if (pathCompare !== 0) {
			return pathCompare;
		}
		return Number(a.source?.line || 0) - Number(b.source?.line || 0);
	});
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	WORDPRESS_HOOK_FUZZ_PLAN_SCHEMA,
	WORDPRESS_HOOK_SURFACE_DISCOVERY_SCHEMA,
	createWordPressHookFuzzPlan,
	discoverWordPressHookSurfaces,
};
