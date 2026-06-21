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
assert.equal(typeof wordpress.normalizeWordPressAjaxActionSurface, 'function');
assert.equal(typeof wordpress.buildAjaxActionPlanArtifact, 'function');
assert.equal(typeof wordpress.normalizeWordPressRestRouteMatrix, 'function');
assert.equal(typeof wordpress.generateWordPressRestRequestCases, 'function');
assert.equal(typeof wordpress.buildFullSurfaceCoverageArtifact, 'function');
assert.equal(typeof wordpress.formatFullSurfaceCoverageMarkdownReport, 'function');
assert.equal(typeof wordpress.normalizeFullSurfaceCoverageManifest, 'function');
assert.equal(typeof wordpress.normalizeFullSurfaceCoverageArtifactRefs, 'function');
assert.equal(typeof wordpress.validateFullSurfaceCoverageArtifacts, 'function');
assert.equal(typeof wordpress.discoverWordPressHookSurfaces, 'function');
assert.equal(typeof wordpress.createWordPressHookFuzzPlan, 'function');
assert.equal(typeof wordpress.normalizeWordPressSurfaceDiscovery, 'function');
assert.equal(typeof wordpress.normalizeWordPressFuzzPlan, 'function');
assert.equal(typeof wordpress.normalizeWordPressFuzzResult, 'function');
assert.equal(typeof wordpress.normalizeWordPressCrudOperation, 'function');
assert.equal(typeof wordpress.normalizeWordPressFixturePersona, 'function');
assert.equal(typeof wordpress.normalizeWordPressPerformanceObservation, 'function');
assert.equal(typeof wordpress.buildWordPressFuzzPlanFromSurfaces, 'function');
assert.equal(typeof wordpress.buildWordPressFuzzRunnerResult, 'function');
assert.equal(typeof wordpress.wpCodebox, 'object');
assert.equal(typeof wordpress.wpCodebox.resolveWpCodeboxArtifactPath, 'function');
assert.equal(typeof wordpress.wpCodebox.runWpCodeboxRecipe, 'function');
assert.equal(typeof wordpress.wpCodebox.buildWpCodeboxFuzzPlanRecipe, 'function');
assert.equal(typeof wordpress.wpCodebox.wpCodeboxFuzzRunTaskRequest, 'function');

assert.equal(typeof wordpress.resolveWpCodeboxArtifactPath, 'function');
assert.equal(typeof wordpress.runWpCodeboxRecipe, 'function');
assert.equal(typeof wordpress.buildWpCodeboxFuzzPlanRecipe, 'function');
assert.equal(typeof wordpress.wpCodeboxFuzzRunTaskRequest, 'function');
assert.equal(typeof wordpress.applyApprovedWpCodeboxArtifact, 'function');
assert.equal(wordpress.compareCodeboxMemoryResults, undefined);

console.log('wordpress public export boundary smoke passed');
