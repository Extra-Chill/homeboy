#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const genericFiles = [
	'docs/agent-runtime-package-contract.md',
	'runtime-agent-ci/lib/agent-task-provider-contract.js',
	'runtime-agent-ci/lib/agent-task-runner-contract.js',
	'tests/fixtures/agent-runtime-manifest.json',
];
const domainTerms = /WordPress|WP Codebox|Data Machine|WPSG|wp-site-generator|datamachine|wordpress|site-generator|site generator/;

const violations = genericFiles.filter((relativePath) => {
	const content = fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
	return domainTerms.test(content);
});

assert.deepEqual(
	violations,
	[],
	`Generic agent runtime files must stay domain-neutral. Violations: ${violations.join(', ')}`,
);

console.log('generic agent runtime boundary smoke passed');
