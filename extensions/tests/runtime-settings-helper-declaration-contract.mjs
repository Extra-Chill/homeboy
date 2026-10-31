#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const RUNTIME_SETTINGS_HELPER_ID = 'runtime-settings';
const RUNTIME_SETTINGS_HELPER_ENV = 'HOMEBOY_RUNTIME_SETTINGS_HELPER';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function phaseDeclaresRuntimeSettings(phase) {
	return Array.isArray(phase?.runtime_helpers)
		&& phase.runtime_helpers.some((helper) => helper && helper.id === RUNTIME_SETTINGS_HELPER_ID);
}

function runnerConsumesRuntimeSettings(scriptText) {
	return typeof scriptText === 'string' && scriptText.includes(RUNTIME_SETTINGS_HELPER_ENV);
}

function missingRuntimeSettingsDeclaration(phase, scriptText) {
	return runnerConsumesRuntimeSettings(scriptText) && !phaseDeclaresRuntimeSettings(phase);
}

assert.equal(
	missingRuntimeSettingsDeclaration(
		{ extension_script: 'scripts/test-runner.sh' },
		`${RUNTIME_SETTINGS_HELPER_ENV}=/path/to/settings.sh`
	),
	true,
	'a consuming runner without runtime-settings must fail the contract'
);
assert.equal(
	missingRuntimeSettingsDeclaration(
		{ runtime_helpers: [{ id: RUNTIME_SETTINGS_HELPER_ID }] },
		`${RUNTIME_SETTINGS_HELPER_ENV}=/path/to/settings.sh`
	),
	false,
	'a consuming runner that declares runtime-settings must pass'
);
assert.equal(
	missingRuntimeSettingsDeclaration({ extension_script: 'scripts/lint-runner.sh' }, 'echo lint'),
	false,
	'a non-consuming runner must not require runtime-settings'
);

function collectExtensionIds(root) {
	return fs.readdirSync(root, { withFileTypes: true })
		.filter((entry) => entry.isDirectory())
		.map((entry) => entry.name)
		.filter((id) => (
			fs.existsSync(path.join(root, id, `${id}.json`))
			&& fs.existsSync(path.join(root, id, 'homeboy.json'))
		))
		.sort();
}

function collectExtensionScriptPhases(value, keyPath = []) {
	const phases = [];
	if (Array.isArray(value)) {
		value.forEach((item, index) => {
			phases.push(...collectExtensionScriptPhases(item, [...keyPath, String(index)]));
		});
		return phases;
	}
	if (!value || typeof value !== 'object') {
		return phases;
	}
	if (typeof value.extension_script === 'string') {
		phases.push({
			keyPath,
			script: value.extension_script,
			phase: value,
		});
	}
	for (const [key, item] of Object.entries(value)) {
		phases.push(...collectExtensionScriptPhases(item, [...keyPath, key]));
	}
	return phases;
}

const failures = [];
const consumingPhases = [];

for (const extensionId of collectExtensionIds(repoRoot)) {
	const manifestPath = path.join(repoRoot, extensionId, `${extensionId}.json`);
	const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
	for (const { keyPath, script, phase } of collectExtensionScriptPhases(manifest)) {
		const scriptPath = path.join(repoRoot, extensionId, script);
		const scriptText = fs.existsSync(scriptPath)
			? fs.readFileSync(scriptPath, 'utf8')
			: '';
		const phaseName = `${extensionId}.${keyPath.join('.')}`;
		if (missingRuntimeSettingsDeclaration(phase, scriptText)) {
			failures.push(
				`${phaseName} owns ${script} which consumes ${RUNTIME_SETTINGS_HELPER_ENV} without declaring ${RUNTIME_SETTINGS_HELPER_ID}`
			);
			continue;
		}
		if (runnerConsumesRuntimeSettings(scriptText)) {
			consumingPhases.push(phaseName);
		}
	}
}

assert.equal(failures.length, 0, failures.join('\n'));
assert.ok(
	consumingPhases.length > 0,
	'expected at least one extension_script phase to consume the runtime-settings helper'
);

console.log(
	`runtime-settings helper declaration contract passed for ${consumingPhases.length} consuming phase(s)`
);
