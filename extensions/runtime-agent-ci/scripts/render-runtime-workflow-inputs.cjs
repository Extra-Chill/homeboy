#!/usr/bin/env node
'use strict';

const {
	renderRuntimeWorkflowInputs,
} = require('../lib/runtime-workflow-inputs.cjs');

function main(argv = process.argv.slice(2), env = process.env) {
	const args = parseArgs(argv);
	const input = args.input ? jsonValue('input', args.input, {}) : {};
	const rendered = renderRuntimeWorkflowInputs({
		...input,
		runtime: firstDefined(args.runtime, env.RUNTIME, input.runtime),
		runtime_profile: firstDefined(parseProfile(args.runtimeProfile), parseProfile(env.PROFILE), input.runtime_profile),
		runtime_profiles: firstDefined(jsonValue('runtime_profiles', args.runtimeProfiles, undefined), jsonValue('runtime_profiles', env.RUNTIME_PROFILES, undefined), input.runtime_profiles),
		tool_profile: firstDefined(jsonValue('tool_profile', args.toolProfile, undefined), jsonValue('tool_profile', env.TOOL_PROFILE, undefined), input.tool_profile),
		runtime_mounts: firstDefined(jsonValue('runtime_mounts', args.runtimeMounts, undefined), jsonValue('runtime_mounts', env.RUNTIME_MOUNTS, undefined), input.runtime_mounts, input.mounts),
		runtime_state_mounts: firstDefined(jsonValue('runtime_state_mounts', args.runtimeStateMounts, undefined), jsonValue('runtime_state_mounts', env.RUNTIME_STATE_MOUNTS, undefined), input.runtime_state_mounts),
		runtime_config_mounts: firstDefined(jsonValue('runtime_config_mounts', args.runtimeConfigMounts, undefined), jsonValue('runtime_config_mounts', env.RUNTIME_CONFIG_MOUNTS, undefined), input.runtime_config_mounts),
	});
	process.stdout.write(`${JSON.stringify(rendered, null, 2)}\n`);
	return rendered;
}

function parseArgs(argv) {
	const args = {};
	for (let index = 0; index < argv.length; index += 1) {
		const token = argv[index];
		if (!token.startsWith('--')) {
			throw new Error(`Unexpected argument: ${token}`);
		}
		const [rawName, inlineValue] = token.slice(2).split(/=(.*)/s, 2);
		const name = camelCase(rawName);
		args[name] = inlineValue === undefined ? argv[++index] : inlineValue;
		if (args[name] === undefined) {
			throw new Error(`Missing value for --${rawName}`);
		}
	}
	return args;
}

function parseProfile(value) {
	if (value === undefined) {
		return undefined;
	}
	const raw = String(value || '').trim();
	if (raw.startsWith('{')) {
		return JSON.parse(raw);
	}
	return raw;
}

function jsonValue(name, value, fallback) {
	if (value === undefined || value === '') {
		return fallback;
	}
	try {
		return JSON.parse(value);
	} catch (error) {
		throw new Error(`${name} must be valid JSON: ${error.message}`);
	}
}

function firstDefined(...values) {
	return values.find((value) => value !== undefined);
}

function camelCase(value) {
	return value.replace(/-([a-z])/g, (_match, letter) => letter.toUpperCase());
}

if (require.main === module) {
	try {
		main();
	} catch (error) {
		process.stderr.write(`${error.message}\n`);
		process.exit(1);
	}
}

module.exports = { main };
