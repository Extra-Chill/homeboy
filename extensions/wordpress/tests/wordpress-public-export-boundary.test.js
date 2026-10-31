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
assert.equal(typeof wordpress.normalizeWordPressRestRouteDiscovery, 'function');
assert.equal(typeof wordpress.discoverWordPressRestRoutes, 'function');
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
assert.equal(typeof wordpress.normalizeWordPressSurfaceType, 'function');
assert.equal(typeof wordpress.normalizeWordPressRuntimeSurfaceType, 'function');
assert.equal(typeof wordpress.normalizeWordPressCoverageSurfaceType, 'function');
assert.equal(typeof wordpress.normalizeWordPressFuzzRuntimeCapabilities, 'function');
assert.equal(typeof wordpress.normalizeWordPressCrudOperation, 'function');
assert.equal(typeof wordpress.normalizeWordPressFixturePersona, 'function');
assert.equal(typeof wordpress.normalizeWordPressPerformanceObservation, 'function');
assert.equal(typeof wordpress.buildWordPressPerformanceObservation, 'function');
assert.equal(typeof wordpress.normalizeWordPressRuntimeSurfaceDiscovery, 'function');
assert.equal(typeof wordpress.buildWordPressRuntimeSurfaceCoverageManifest, 'function');
assert.equal(typeof wordpress.buildWordPressFuzzPlanFromSurfaces, 'function');
assert.equal(typeof wordpress.discoverWooCommercePublicContracts, 'function');
assert.equal(wordpress.WOOCOMMERCE_PUBLIC_CONTRACT_DISCOVERY_SCHEMA, 'homeboy/woocommerce-public-contract-discovery/v1');
assert.equal(typeof wordpress.createHomeboyPublicContractEvidence, 'function');
assert.equal(typeof wordpress.toFinalizationEvidencePolicy, 'function');
assert.equal(wordpress.HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA, 'homeboy/public-contract-evidence/v1');
assert.equal(typeof wordpress.compileWordPressFuzzCampaign, 'function');
assert.equal(typeof wordpress.runWordPressFuzzCampaign, 'function');
assert.equal(typeof wordpress.validateWordPressFuzzCampaignArtifacts, 'function');
assert.equal(typeof wordpress.detectWordPressFuzzPlanResultGaps, 'function');
assert.equal(typeof wordpress.buildWordPressFuzzRunnerResult, 'function');
assert.equal(typeof wordpress.buildWordPressFuzzRuntimeTaskRequest, 'function');
assert.equal(typeof wordpress.normalizeFuzzHotspotSummary, 'function');
assert.equal(wordpress.WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA, 'homeboy-extension-wordpress/dependency-materialization-recipes/v1');
assert.equal(wordpress.WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA, 'homeboy-extension-wordpress/required-output-declarations/v1');
assert.equal(typeof wordpress.wpCodebox, 'object');
assert.equal(typeof wordpress.wpCodebox.resolveWpCodeboxArtifactPath, 'function');
assert.equal(typeof wordpress.wpCodebox.runWpCodeboxRecipe, 'function');
assert.equal(typeof wordpress.wpCodebox.buildWpCodeboxFuzzPlanRecipe, 'function');
assert.equal(typeof wordpress.wpCodebox.wpCodeboxFuzzExecutionRequest, 'function');

assert.equal(wordpress.resolveWpCodeboxArtifactPath, undefined);
assert.equal(wordpress.runWpCodeboxRecipe, undefined);
assert.equal(wordpress.buildWpCodeboxFuzzPlanRecipe, undefined);
assert.equal(wordpress.wpCodeboxFuzzExecutionRequest, undefined);
assert.equal(wordpress.WP_CODEBOX_FUZZ_SUITE_SCHEMA, undefined);
assert.equal(wordpress.applyApprovedWpCodeboxArtifact, undefined);
assert.equal(wordpress.compareCodeboxMemoryResults, undefined);
assert.equal(wordpress.createStaticSiteFanoutPlan, undefined);

console.log('wordpress public export boundary smoke passed');
