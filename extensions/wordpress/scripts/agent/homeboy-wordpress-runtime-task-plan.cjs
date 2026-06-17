#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const { wordpressRuntimeTaskPlan } = require('../../lib/wordpress-runtime-task-planner');

function argValue(name) {
	const index = process.argv.indexOf(name);
	return index >= 0 ? process.argv[index + 1] : '';
}

function argValues(name) {
	const values = [];
	for (let index = 0; index < process.argv.length; index += 1) {
		if (process.argv[index] === name && process.argv[index + 1]) {
			values.push(process.argv[index + 1]);
		}
	}
	return values;
}

function hasFlag(name) {
	return process.argv.includes(name);
}

function usage() {
	console.error('Usage: homeboy-wordpress-runtime-task-plan.cjs --plan-id <id> --ability <ability> [--ability-input <json>] [--ability-input-file <file>] [--task-id <id>] [--backend <id>] [--provider <id>] [--model <id>] [--runtime <id>] [--runtime-id <id>] [--runtime-bin <path>] [--dla-url <url>] [--expected-artifact <kind>] [--timeout-seconds <n>] [--concurrency <n>] [--fanout <json-array>] [--fanout-file <file>] [--source-ref <json>] [--metadata <json>] [--config <json>] [--output <plan.json>]');
	process.exit(1);
}

function readJsonArg(value, label) {
	if (!value) {
		return undefined;
	}
	try {
		return JSON.parse(value);
	} catch (error) {
		throw new Error(`${label} must be valid JSON: ${error.message}`);
	}
}

function readJsonFile(filePath, label) {
	if (!filePath) {
		return undefined;
	}
	return readJsonArg(fs.readFileSync(filePath, 'utf8'), label);
}

function writeJson(filePath, payload) {
	fs.mkdirSync(path.dirname(filePath), { recursive: true });
	fs.writeFileSync(filePath, `${JSON.stringify(payload, null, 2)}\n`);
}

const planId = argValue('--plan-id');
const ability = argValue('--ability');
if (!planId || !ability || hasFlag('--help') || hasFlag('-h')) {
	usage();
}

try {
	const abilityInput = readJsonFile(argValue('--ability-input-file'), 'ability input file')
		|| readJsonArg(argValue('--ability-input'), 'ability input')
		|| {};
	const fanout = readJsonFile(argValue('--fanout-file'), 'fanout file')
		|| readJsonArg(argValue('--fanout'), 'fanout');
	const plan = wordpressRuntimeTaskPlan({
		planId,
		ability,
		taskId: argValue('--task-id') || undefined,
		abilityInput,
		backend: argValue('--backend') || undefined,
		provider: argValue('--provider') || undefined,
		model: argValue('--model') || undefined,
		runtime: argValue('--runtime') || undefined,
		runtimeId: argValue('--runtime-id') || undefined,
		runtimeBin: argValue('--runtime-bin') || undefined,
		dlaUrl: argValue('--dla-url') || undefined,
		instructions: argValue('--instructions') || undefined,
		repo: argValue('--repo') || undefined,
		cwd: argValue('--cwd') || undefined,
		groupKey: argValue('--group-key') || undefined,
		expectedArtifacts: argValues('--expected-artifact'),
		providerPluginPaths: argValues('--provider-plugin-path'),
		secretEnv: argValues('--secret-env'),
		timeoutSeconds: argValue('--timeout-seconds') || undefined,
		concurrency: argValue('--concurrency') || undefined,
		fanout,
		sourceRefs: argValues('--source-ref').map((value) => readJsonArg(value, 'source ref')),
		metadata: readJsonArg(argValue('--metadata'), 'metadata') || undefined,
		config: readJsonArg(argValue('--config'), 'config') || undefined,
	});

	const output = argValue('--output');
	if (output) {
		writeJson(output, plan);
	}
	console.log(JSON.stringify(plan, null, 2));
} catch (error) {
	console.error(error && error.stack ? error.stack : String(error));
	process.exit(1);
}
