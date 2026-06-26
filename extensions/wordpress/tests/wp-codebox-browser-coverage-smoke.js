'use strict';

const assert = require('node:assert/strict');
const { existsSync } = require('node:fs');
const { mkdtemp, mkdir, readFile, rm, writeFile } = require('node:fs/promises');
const { tmpdir } = require('node:os');
const path = require('node:path');
const {
	WP_CODEBOX_BROWSER_COVERAGE_SCHEMA,
	normalizeWpCodeboxBrowserCoveragePrimitive,
	runWpCodeboxBrowserCoverageTrace,
} = require('../lib/wp-codebox-browser-coverage');

const declaration = normalizeWpCodeboxBrowserCoveragePrimitive({
	componentId: 'woocommerce',
	requiredFile: 'woocommerce.php',
	activationFile: 'woocommerce.php',
	scenarios: [
		{
			id: 'shop',
			label: 'Shop page',
			stepsFile: 'browser-scenarios/shop.json',
			tags: ['frontend', ' request-coverage '],
			metadata: { route: '/shop/' },
		},
	],
	profile: {
		wpVersion: '7.0',
		viewport: '1366x900',
		stepTimeout: '45s',
		timeout: '180s',
		blueprintSteps: [{ step: 'login', username: 'admin', password: 'password' }],
		inputs: { pluginRuntime: { mode: 'local' } },
		assumptions: ['logged-in admin'],
	},
	profileMetadata: { owner: 'fuzz', coverage: 'browser-request' },
	traceCommand: {
		command: 'node',
		args: ['bench/woocommerce-browser-coverage.trace.mjs'],
		cwd: '${component.root}',
		env: { HOMEBOY_TRACE_SCENARIO: 'shop' },
	},
});

assert.equal(declaration.schema, WP_CODEBOX_BROWSER_COVERAGE_SCHEMA);
assert.equal(declaration.component_id, 'woocommerce');
assert.equal(declaration.required_file, 'woocommerce.php');
assert.equal(declaration.activation_file, 'woocommerce.php');
assert.deepEqual(declaration.scenarios, [
	{
		id: 'shop',
		label: 'Shop page',
		steps_file: 'browser-scenarios/shop.json',
		tags: ['frontend', 'request-coverage'],
		metadata: { route: '/shop/' },
	},
]);
assert.deepEqual(declaration.profile, {
	wp_version: '7.0',
	viewport: '1366x900',
	step_timeout: '45s',
	timeout: '180s',
	blueprint_steps: [{ step: 'login', username: 'admin', password: 'password' }],
	inputs: { pluginRuntime: { mode: 'local' } },
	assumptions: ['logged-in admin'],
});
assert.deepEqual(declaration.profile_metadata, { coverage: 'browser-request', owner: 'fuzz' });
assert.deepEqual(declaration.trace_command, {
	command: 'node',
	argv: ['bench/woocommerce-browser-coverage.trace.mjs'],
	cwd: '${component.root}',
	env: { HOMEBOY_TRACE_SCENARIO: 'shop' },
});

assert.deepEqual(normalizeWpCodeboxBrowserCoveragePrimitive({
	component_id: 'jetpack',
	scenarios: [{ scenario_id: 'dashboard' }],
	trace_command: 'node bench/jetpack-browser-coverage.trace.mjs',
}).trace_command, { command: 'node bench/jetpack-browser-coverage.trace.mjs' });

assert.throws(
	() => normalizeWpCodeboxBrowserCoveragePrimitive({ scenarios: [{ id: 'shop' }], trace_command: 'node trace.mjs' }),
	/requires component_id/
);
assert.throws(
	() => normalizeWpCodeboxBrowserCoveragePrimitive({ component_id: 'woocommerce', trace_command: 'node trace.mjs' }),
	/requires at least one scenario/
);
assert.throws(
	() => normalizeWpCodeboxBrowserCoveragePrimitive({ component_id: 'woocommerce', scenarios: [{ id: 'shop' }] }),
	/requires trace_command/
);

async function runLifecycleSmoke() {
  const workspace = await mkdtemp(path.join(tmpdir(), 'wp-codebox-browser-coverage-smoke.'));
  const stepsFile = path.join(workspace, 'steps.json');
  const resultsFile = path.join(workspace, 'results', 'trace.json');
  const artifactDir = path.join(workspace, 'artifacts');
  const componentPath = path.join(workspace, 'component');

  await mkdir(componentPath, { recursive: true });
  await writeFile(path.join(componentPath, 'plugin.php'), '<?php');
  await writeFile(stepsFile, `${JSON.stringify([{ action: 'goto', url: '/' }])}\n`);

  try {
    const traceResult = await runWpCodeboxBrowserCoverageTrace({
      componentId: 'sample-plugin',
      scenarioId: 'sample-browser-coverage',
      resultsFile,
      artifactDir,
      componentPath,
      requiredFile: 'plugin.php',
      setupCode: '<?php update_option( "homeboy_smoke", "1" );',
      assumptions: ['fake recipe runner produces browser artifacts'],
      scenarios: [{ id: 'front', stepsFile }],
      runRecipe: async ({ recipeFile, artifactsDir, outputFile }) => {
        const recipe = JSON.parse(await readFile(recipeFile, 'utf8'));
        assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
        assert.equal(recipe.workflow.steps.length, 2);
        assert.equal(recipe.workflow.steps[1].command, 'wordpress.browser-actions');

        const browserDir = path.join(artifactsDir, 'files', 'browser');
        await mkdir(browserDir, { recursive: true });
        await writeFile(path.join(browserDir, 'action-summary.json'), `${JSON.stringify({ finalUrl: 'https://example.test/' })}\n`);
        await writeFile(path.join(browserDir, 'request-coverage.json'), `${JSON.stringify({ schema: 'homeboy/browser-request-coverage/v1' })}\n`);
        await writeFile(path.join(browserDir, 'network.jsonl'), `${JSON.stringify({ type: 'request' })}\n${JSON.stringify({ type: 'response' })}\n`);
        await writeFile(path.join(browserDir, 'errors.jsonl'), '');
        await writeFile(path.join(browserDir, 'steps.jsonl'), `${JSON.stringify({ status: 'passed' })}\n`);

        const output = { artifacts: { directory: artifactsDir } };
        await writeFile(outputFile, `${JSON.stringify(output)}\n`);
        return { stdout: JSON.stringify(output), json: output };
      },
    });

    const writtenResult = JSON.parse(await readFile(resultsFile, 'utf8'));
    assert.equal(traceResult.status, 'pass');
    assert.equal(writtenResult.status, 'pass');
    assert.equal(writtenResult.component_id, 'sample-plugin');
    assert.equal(writtenResult.scenario_id, 'sample-browser-coverage');
    assert.match(writtenResult.summary, /1 scenario\(s\), 2 network event\(s\), 1 response/);
    assert.deepEqual(writtenResult.assertions.map((item) => item.status), ['pass', 'pass', 'pass']);
    assert.equal(writtenResult.metadata.final_url, 'https://example.test/');
    assert.equal(writtenResult.metadata.request_coverage_schema, 'homeboy/browser-request-coverage/v1');
    assert.ok(writtenResult.artifacts.some((artifact) => artifact.path.endsWith('request-coverage.json')));
    assert.ok(existsSync(path.join(artifactDir, 'wp-codebox-output.json')));
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
}

runLifecycleSmoke().then(() => {
  console.log('WP Codebox browser coverage primitive smoke passed.');
});
