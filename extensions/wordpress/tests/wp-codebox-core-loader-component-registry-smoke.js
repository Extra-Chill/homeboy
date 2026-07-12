'use strict';

/**
 * Smoke test: wp-codebox-core-loader.js falls back to the Homeboy
 * component registry (`homeboy component show wp-codebox`) when the
 * checkout can't be found by any of the earlier discovery mechanisms
 * (explicit override, npm packages, HOMEBOY_WP_CODEBOX_INSTALL_DIR cache,
 * or a sibling checkout under workspaceRoots()).
 *
 * This exercises the same discovery path homeboy-extensions#2213 needed:
 * a DMC workspace root (e.g. /var/lib/datamachine/workspace) that differs
 * from the wordpress extension's own parent directory, but where
 * `wp-codebox` is still resolvable through the Homeboy component registry.
 */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
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
	//    should include it via the fake `homeboy component show` binary.
	const registeredRoot = path.join(root, 'registered-wp-codebox');
	const runtimeCoreDist = path.join(registeredRoot, 'packages', 'runtime-core', 'dist');
	fs.mkdirSync(runtimeCoreDist, { recursive: true });
	fs.writeFileSync(path.join(runtimeCoreDist, 'recipe-builders.js'), 'export function buildWordPressPhpunitRecipe() { return {}; }\n');

	const fakeHomeboyBin = path.join(root, 'fake-homeboy.sh');
	fs.writeFileSync(fakeHomeboyBin, [
		'#!/bin/sh',
		'if [ "$1" = "component" ] && [ "$2" = "show" ] && [ "$3" = "wp-codebox" ]; then',
		`  printf '%s' '${JSON.stringify({ data: { entity: { local_path: registeredRoot } } } )}'`,
		'  exit 0',
		'fi',
		'exit 1',
	].join('\n'));
	fs.chmodSync(fakeHomeboyBin, 0o755);

	const candidates = coreModuleCandidates({
		...OPTIONS,
		homeboyBin: fakeHomeboyBin,
		workspaceRoot: path.join(root, 'empty-workspace-root'),
	});

	const expectedCandidate = fileUrl(path.resolve(registeredRoot, 'packages/runtime-core/dist/recipe-builders.js'));
	assert.ok(
		candidates.includes(expectedCandidate),
		`expected component-registry candidate ${expectedCandidate} in ${JSON.stringify(candidates)}`
	);

	// 2. When workspaceRoots() sibling-checkout discovery already found a
	//    candidate, the component-registry fallback must not run (so it
	//    can't shell out unnecessarily or override an already-found path).
	const siblingWorkspaceRoot = path.join(root, 'sibling-workspace');
	const siblingModule = path.join(siblingWorkspaceRoot, 'wp-codebox', 'packages', 'runtime-core', 'dist', 'recipe-builders.js');
	fs.mkdirSync(path.dirname(siblingModule), { recursive: true });
	fs.writeFileSync(siblingModule, 'export function buildWordPressPhpunitRecipe() { return {}; }\n');

	const unreachableHomeboyBin = path.join(root, 'unreachable-homeboy.sh');
	fs.writeFileSync(unreachableHomeboyBin, '#!/bin/sh\necho "should not be called" >&2\nexit 1\n');
	fs.chmodSync(unreachableHomeboyBin, 0o755);

	const siblingCandidates = coreModuleCandidates({
		...OPTIONS,
		homeboyBin: unreachableHomeboyBin,
		workspaceRoot: siblingWorkspaceRoot,
	});
	const expectedSiblingCandidate = fileUrl(siblingModule);
	assert.ok(siblingCandidates.includes(expectedSiblingCandidate), 'sibling checkout candidate should still be discovered');

	// 3. When `homeboy` is unavailable/unregistered, discovery degrades
	//    gracefully (no candidate added, no throw).
	const missingHomeboyBin = path.join(root, 'missing-homeboy.sh');
	fs.writeFileSync(missingHomeboyBin, '#!/bin/sh\nexit 1\n');
	fs.chmodSync(missingHomeboyBin, 0o755);

	const gracefulCandidates = coreModuleCandidates({
		...OPTIONS,
		homeboyBin: missingHomeboyBin,
		workspaceRoot: path.join(root, 'another-empty-workspace-root'),
	});
	assert.ok(!gracefulCandidates.some((c) => c.includes('registered-wp-codebox')), 'no stray candidates when homeboy lookup fails');

	console.log('wp-codebox core loader component registry smoke passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
