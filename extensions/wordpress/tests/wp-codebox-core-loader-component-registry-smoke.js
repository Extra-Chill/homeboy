'use strict';

/**
 * Smoke test: wp-codebox-core-loader.js falls back to non-evaluating
 * Homeboy component registry metadata when the
 * checkout can't be found by any of the earlier discovery mechanisms
 * (explicit override, npm packages, HOMEBOY_WP_CODEBOX_INSTALL_DIR cache,
 * or a sibling checkout under workspaceRoots()).
 *
 * This exercises the same discovery path homeboy-extensions#2213 needed:
 * a DMC workspace root (e.g. /var/lib/datamachine/workspace) that differs
 * from the wordpress extension's own parent directory, but where
 * `wp-codebox` is still resolvable through the Homeboy component registry.
 * It also models the production recursion path with a bounded fake Homeboy
 * command that would re-run the real readiness check if invoked.
 */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const { coreModuleCandidates } = require('../lib/wp-codebox-core-loader');

function fileUrl(absolutePath) {
	return pathToFileURL(absolutePath).href;
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-component-registry-'));

const OPTIONS = {
	packageCandidates: [
		'@automattic/wp-codebox-core/recipe-builders',
		'wp-codebox-workspace/recipe-builders',
	],
	packageDistEntries: ['recipe-builders.js', 'index.js'],
	runtimeCoreEntries: ['packages/runtime-core/dist/recipe-builders.js', 'packages/runtime-core/dist/index.js'],
	includeGlobalNodeModuleRoots: false,
};

try {
	// 1. Registered checkout with a valid built runtime-core entry: discovery
	//    should include it directly from Homeboy's standalone registry metadata.
	const registeredRoot = path.join(root, 'registered-wp-codebox');
	const runtimeCoreDist = path.join(registeredRoot, 'packages', 'runtime-core', 'dist');
	fs.mkdirSync(runtimeCoreDist, { recursive: true });
	fs.writeFileSync(path.join(runtimeCoreDist, 'recipe-builders.js'), 'exports.buildWordPressBenchRecipe = () => ({});\n');

	const homeDir = path.join(root, 'home');
	const registryDir = path.join(homeDir, '.config', 'homeboy', 'components');
	fs.mkdirSync(registryDir, { recursive: true });
	fs.writeFileSync(path.join(registryDir, 'wp-codebox.json'), JSON.stringify({ local_path: registeredRoot }));

	const invocationLog = path.join(root, 'homeboy-invocations.log');
	const recursiveHomeboyBin = path.join(root, 'recursive-homeboy.sh');
	const readyScript = path.join(__dirname, '..', 'scripts', 'build', 'check-wp-codebox-runtime-core.mjs');
	fs.writeFileSync(recursiveHomeboyBin, [
		'#!/bin/sh',
		'if [ "$1" = "component" ] && [ "$2" = "show" ] && [ "$3" = "wp-codebox" ]; then',
		'  printf "invoked\\n" >> "$HOMEBOY_RECURSION_LOG"',
		'  depth=${HOMEBOY_RECURSION_DEPTH:-0}',
		'  [ "$depth" -ge 1 ] && exit 1',
		'  HOMEBOY_RECURSION_DEPTH=$((depth + 1)) exec node "$HOMEBOY_READY_SCRIPT"',
		'fi',
		'exit 1',
	].join('\n'));
	fs.chmodSync(recursiveHomeboyBin, 0o755);

	const candidates = coreModuleCandidates({
		...OPTIONS,
		homeboyComponentRegistryDir: registryDir,
		workspaceRoot: path.join(root, 'empty-workspace-root'),
	});

	const expectedCandidate = fileUrl(path.resolve(registeredRoot, 'packages/runtime-core/dist/recipe-builders.js'));
	assert.ok(
		candidates.includes(expectedCandidate),
		`expected component-registry candidate ${expectedCandidate} in ${JSON.stringify(candidates)}`
	);

	const readiness = spawnSync(process.execPath, [readyScript], {
		cwd: path.join(__dirname, '..'),
		encoding: 'utf8',
		timeout: 3000,
		killSignal: 'SIGKILL',
		env: {
			...process.env,
			HOME: homeDir,
			HOMEBOY_BIN: recursiveHomeboyBin,
			HOMEBOY_READY_SCRIPT: readyScript,
			HOMEBOY_RECURSION_LOG: invocationLog,
			HOMEBOY_RECURSION_DEPTH: '0',
			HOMEBOY_WORKSPACE_ROOT: path.join(root, 'empty-workspace-root'),
			HOMEBOY_DEVELOPER_WORKSPACE: path.join(root, 'empty-developer-workspace'),
			HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(root, 'empty-install-cache'),
			HOMEBOY_GLOBAL_NODE_MODULE_ROOT: path.join(root, 'empty-global-modules'),
			HOMEBOY_WP_CODEBOX_CORE_MODULE: '',
			WP_CODEBOX_CORE_MODULE: '',
		},
	});
	assert.equal(readiness.error, undefined, readiness.error?.message);
	assert.equal(readiness.status, 0, readiness.stderr);
	assert.match(readiness.stdout, /WP Codebox runtime core ready:/);
	assert.equal(fs.existsSync(invocationLog), false, 'readiness must not invoke component inspection');

	// 2. When workspaceRoots() sibling-checkout discovery already found a
	//    candidate, the component-registry fallback must not run (so it
	//    can't override an already-found path).
	const siblingWorkspaceRoot = path.join(root, 'sibling-workspace');
	const siblingModule = path.join(siblingWorkspaceRoot, 'wp-codebox', 'packages', 'runtime-core', 'dist', 'recipe-builders.js');
	fs.mkdirSync(path.dirname(siblingModule), { recursive: true });
	fs.writeFileSync(siblingModule, 'export function buildWordPressPhpunitRecipe() { return {}; }\n');

	const siblingCandidates = coreModuleCandidates({
		...OPTIONS,
		homeboyComponentRegistryDir: registryDir,
		workspaceRoot: siblingWorkspaceRoot,
	});
	const expectedSiblingCandidate = fileUrl(siblingModule);
	assert.ok(siblingCandidates.includes(expectedSiblingCandidate), 'sibling checkout candidate should still be discovered');
	assert.ok(!siblingCandidates.includes(expectedCandidate), 'registry fallback should not run after sibling discovery succeeds');

	// 3. When registry metadata is unavailable, discovery degrades
	//    gracefully (no candidate added, no throw).
	const gracefulCandidates = coreModuleCandidates({
		...OPTIONS,
		homeboyComponentRegistryDir: path.join(root, 'missing-registry'),
		workspaceRoot: path.join(root, 'another-empty-workspace-root'),
	});
	assert.ok(!gracefulCandidates.some((c) => c.includes('registered-wp-codebox')), 'no stray candidates when registry lookup fails');

	console.log('wp-codebox core loader component registry smoke passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
