'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const wordpress = require('../index');

assert.equal(typeof wordpress.profileWordPressAdminPageScenario, 'function');
assert.equal(typeof wordpress.wpCodebox, 'object');
assert.equal(typeof wordpress.wpCodebox.resolveWpCodeboxArtifactPath, 'function');
assert.equal(typeof wordpress.wpCodebox.runWpCodeboxRecipe, 'function');

assert.equal(wordpress.resolveWpCodeboxArtifactPath, undefined);
assert.equal(wordpress.runWpCodeboxRecipe, undefined);
assert.equal(wordpress.applyApprovedWpCodeboxArtifact, undefined);
assert.equal(wordpress.compareCodeboxMemoryResults, undefined);

console.log('wordpress public export boundary smoke passed');
