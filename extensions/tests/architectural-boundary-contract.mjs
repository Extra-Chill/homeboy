#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const runtimePackage = require(path.join(repoRoot, 'agent-runtimes', 'wp-codebox'));
const wpCodeboxPackageJson = JSON.parse(fs.readFileSync(path.join(repoRoot, 'agent-runtimes', 'wp-codebox', 'package.json'), 'utf8'));

assert.equal(runtimePackage.wpCodeboxCliDescriptor().schema, 'wp-codebox/cli-descriptor/v1');

const stableWpCodeboxConsumerExports = [
	'.',
	'./codebox-agent-task-executor',
	'./codebox-artifact-contract',
	'./codebox-run-agent-task-contract',
	'./codebox-runtime-profile',
	'./provider-credential-boundary',
	'./provider-outcome-normalizer',
	'./wp-codebox-adapter-contract',
	'./wp-codebox-adapter-descriptor',
	'./wp-codebox-runtime-contract-source',
	'./wp-codebox-runtime-readiness',
];
for (const exportName of stableWpCodeboxConsumerExports) {
	assert.ok(wpCodeboxPackageJson.exports[exportName], `${exportName} stays exported for runtime consumers`);
}

const wordpressPackage = await import(path.join(repoRoot, 'wordpress/index.js'));
assert.equal(typeof wordpressPackage.default.auditFanoutRuntimeProviderInterface, 'function');
assert.equal(wordpressPackage.default.createAuditWpCodeboxFanoutPlan, undefined);
assert.equal(wordpressPackage.default.wpCodebox?.createAuditWpCodeboxFanoutPlan, undefined);

console.log('architectural boundary contract passed');
