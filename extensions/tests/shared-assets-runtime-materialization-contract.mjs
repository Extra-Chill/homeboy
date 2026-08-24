#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

/**
 * Regression guard for Extra-Chill/homeboy#7736.
 *
 * The three CLI agent-task executor runtimes (opencode/codex/claude-code, plus
 * wp-codebox and pi) each `require('../../../agent-task-contracts')`, which from
 * an installed runtime at `~/.config/homeboy/agent-runtimes/<runtime>/lib/`
 * resolves to `~/.config/homeboy/agent-task-contracts/`. That directory only
 * exists on an install if `agent-task-contracts` is declared as a shared asset
 * that Homeboy core materializes alongside `agent-runtimes` and
 * `runtime-agent-ci`.
 *
 * The materialization copy loop itself lives in the Rust `Extra-Chill/homeboy`
 * binary; this repo owns the declarative enumeration in
 * `homeboy-extension-root.json` `shared_assets`. When `agent-task-contracts`
 * was missing from that enumeration (or from core's transitional fallback list)
 * the package never landed on the install and every CLI backend crashed with
 * `MODULE_NOT_FOUND` before the agent ran (confirming run id
 * `agent-task-ed341028-b0db-424e-a165-986241430ad7`).
 *
 * This test derives the required shared-asset set from the actual cross-package
 * `require()` graph rather than hardcoding it, so any new top-level shared
 * package a runtime reaches for must be declared in `shared_assets` or this
 * test fails.
 */

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const rootManifest = JSON.parse(
	fs.readFileSync(path.join(repoRoot, 'homeboy-extension-root.json'), 'utf8')
);

assert.ok(
	Array.isArray(rootManifest.shared_assets),
	'homeboy-extension-root.json must declare a shared_assets array'
);
const declaredSharedAssets = new Set(rootManifest.shared_assets);
const installedExtensions = new Set(
	fs.readdirSync(repoRoot, { withFileTypes: true })
		.filter((entry) => entry.isDirectory() && fs.existsSync(path.join(repoRoot, entry.name, 'homeboy.json')))
		.map((entry) => entry.name)
);

// The runtime packages whose require graphs must resolve on an install. These
// are themselves declared shared assets that Homeboy core materializes.
const runtimeSharedAssets = ['agent-runtimes', 'runtime-agent-ci'];
for (const asset of runtimeSharedAssets) {
	assert.ok(
		declaredSharedAssets.has(asset),
		`shared_assets must declare the "${asset}" runtime package`
	);
}

// Recursively collect the runtime JS/CJS/MJS files inside a shared-asset
// package, skipping vendored dependencies, fixtures, and tests. Only runtime
// code is materialized onto an install and executed by Homeboy core, so only
// runtime requires define the co-materialization invariant; test files may
// reach into repo-only paths (e.g. CI scripts under .github) that never ship.
function collectSourceFiles(dir) {
	const files = [];
	for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
		if (
			entry.name === 'node_modules' ||
			entry.name === '.git' ||
			entry.name === 'tests' ||
			entry.name === 'fixtures'
		) {
			continue;
		}
		const full = path.join(dir, entry.name);
		if (entry.isDirectory()) {
			files.push(...collectSourceFiles(full));
		} else if (/\.(js|cjs|mjs)$/.test(entry.name) && !/\.test\./.test(entry.name)) {
			files.push(full);
		}
	}
	return files;
}

// Extract require() specifiers from a source file.
const REQUIRE_PATTERN = /require\(\s*['"]([^'"]+)['"]\s*\)/g;
function requireSpecifiers(file) {
	const source = fs.readFileSync(file, 'utf8');
	const specifiers = [];
	let match;
	while ((match = REQUIRE_PATTERN.exec(source)) !== null) {
		specifiers.push(match[1]);
	}
	return specifiers;
}

// Walk every runtime shared-asset package and record which OTHER top-level repo
// directories it reaches into via relative requires. Each dependency must be a
// shared asset or an extension that Homeboy installs under extensions/<id>.
const referencedSharedAssets = new Set();
for (const asset of runtimeSharedAssets) {
	const assetDir = path.join(repoRoot, asset);
	for (const file of collectSourceFiles(assetDir)) {
		for (const specifier of requireSpecifiers(file)) {
			if (!specifier.startsWith('.')) {
				continue; // package/builtin, not a repo-relative reference
			}
			const resolvedDir = path.resolve(path.dirname(file), specifier);
			const relFromRoot = path.relative(repoRoot, resolvedDir);
			if (relFromRoot.startsWith('..') || path.isAbsolute(relFromRoot)) {
				continue; // escapes the repo — not our concern here
			}
			const topLevel = relFromRoot.split(path.sep)[0];
			if (!topLevel || topLevel === asset) {
				continue; // intra-package reference
			}
			referencedSharedAssets.add(topLevel);
		}
	}
}

// Every top-level dependency must have an installed materialization contract.
// Shared packages are copied beside agent-runtimes; extension dependencies are
// copied beneath extensions and must resolve that distinct installed layout.
for (const referenced of [...referencedSharedAssets].sort()) {
	assert.ok(
		declaredSharedAssets.has(referenced) || installedExtensions.has(referenced),
		`runtime shared assets require top-level "${referenced}", so it must be declared in ` +
			'homeboy-extension-root.json shared_assets or be an installable extension with homeboy.json. ' +
			'Otherwise the dependency is absent after extension materialization.'
	);
}

// The specific package that regressed in #7736.
assert.ok(
	referencedSharedAssets.has('agent-task-contracts'),
	'expected the runtime shared assets to require agent-task-contracts'
);
assert.ok(
	declaredSharedAssets.has('agent-task-contracts'),
	'agent-task-contracts must remain a declared shared asset (Extra-Chill/homeboy#7736)'
);

// Prove the CLI agent-task executor require graphs actually resolve against the
// intended installed layout: agent-task-contracts and runtime-agent-ci are
// siblings under the install root, exactly as they are siblings under repoRoot.
const executorEntrypoints = [
	'agent-runtimes/lib/cli-agent-task-executor.js',
	'agent-runtimes/opencode/lib/opencode-agent-task-executor.js',
	'agent-runtimes/codex/lib/codex-agent-task-executor.js',
	'agent-runtimes/claude-code/lib/claude-code-agent-task-executor.js',
	'agent-runtimes/pi/lib/pi-agent-task-executor.js',
];
for (const entrypoint of executorEntrypoints) {
	const full = path.join(repoRoot, entrypoint);
	assert.doesNotThrow(
		() => require(full),
		`${entrypoint} must load with agent-task-contracts resolvable as a sibling shared asset`
	);
}

// The contract package must expose the surface the executors import from it.
const contractsIndex = path.join(repoRoot, 'agent-task-contracts', 'index.js');
assert.ok(
	fs.existsSync(contractsIndex),
	'agent-task-contracts/index.js must exist as the materialized package entrypoint'
);
const contracts = require(contractsIndex);
for (const symbol of ['AGENT_TASK_REQUEST_SCHEMA', 'AGENT_TASK_OUTCOME_SCHEMA']) {
	assert.ok(
		symbol in contracts,
		`agent-task-contracts must export ${symbol} for runtime executors`
	);
}

console.log('shared-assets runtime materialization contract passed');
