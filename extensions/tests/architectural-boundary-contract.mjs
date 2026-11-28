#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const runtimePackage = require(path.join(repoRoot, 'wordpress', 'lib', 'wp-codebox-runtime-contract-source.js'));
// Resolve the public subpaths from inside the package using Node's self-reference.
const requireWordPress = createRequire(path.join(repoRoot, 'wordpress', 'package.json'));

assert.equal(runtimePackage.runtimeContractManifest().schema, 'wp-codebox/runtime-contract-manifest/v1');

const stableWpCodeboxConsumerExports = [
	'.',
	'./wp-codebox-artifact-contract',
	'./wp-codebox-runtime-profile',
	'./wp-codebox-runtime-contract-source',
	'./wp-codebox-runtime-readiness',
];
for (const exportName of stableWpCodeboxConsumerExports) {
	const specifier = exportName === '.' ? 'homeboy-extension-wordpress' : `homeboy-extension-wordpress/${exportName.slice(2)}`;
	requireWordPress(specifier);
}

const wordpressPackage = await import(path.join(repoRoot, 'wordpress/index.js'));
assert.equal(wordpressPackage.default.auditFanoutRuntimeProviderInterface, undefined);

console.log('architectural boundary contract passed');
