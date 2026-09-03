'use strict';

const assert = require('node:assert/strict');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(
	__dirname,
	'..',
	'..',
	'tests',
	'fixtures',
	'wp-codebox-core-runtime-contract.cjs'
);

const {
	artifactRoleFromCodeboxArtifact,
	normalizeCodeboxArtifactOutcome,
} = require('../lib/wp-codebox-artifact-contract');

assert.equal(artifactRoleFromCodeboxArtifact({ name: 'patch.diff', path: 'files/patch.diff' }), 'artifact');
assert.equal(artifactRoleFromCodeboxArtifact({ role: 'patch', name: 'opaque-output' }), 'patch');
assert.equal(artifactRoleFromCodeboxArtifact({ kind: 'screenshot', name: 'opaque-output' }), 'screenshot');
assert.equal(artifactRoleFromCodeboxArtifact({ name: 'opaque-output' }, { artifact_filenames: { patch: ['opaque-output'] } }), 'artifact');
assert.equal(normalizeCodeboxArtifactOutcome({ name: 'result', path: 'result.json' }, { name: 'patch.diff' }).role, 'artifact');

console.log('wp-codebox artifact contract boundary passed');
