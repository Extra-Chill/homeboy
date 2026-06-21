'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	fixtureRecipeStep,
	installWordPressFixturePlugins,
	normalizeFixtureList,
	normalizeFixturePluginList,
	restoreWordPressFixturePlugins,
	runWordPressFixtureSetup,
	withWordPressFixturePlugins,
} = require('../lib/fixture-setup');
const { wpCodeboxPluginStateStep } = require('../lib/wp-codebox-recipe-helper');

const fixtureDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-fixtures-'));

async function main() {
	try {
		assert.deepEqual(
			normalizeFixtureList([{ path: 'fixtures/seed.php' }]).map((step) => ({ type: step.type, label: step.label })),
			[{ type: 'wp-eval-file', label: 'wp-eval-file:1' }]
		);
		assert.deepEqual(
			normalizeFixturePluginList(['/tmp/example-plugin']).map((plugin) => ({ slug: plugin.slug, plugin: plugin.plugin, activate: plugin.activate })),
			[{ slug: 'example-plugin', plugin: 'example-plugin', activate: true }]
		);

		const calls = [];
		const result = await runWordPressFixtureSetup({
			artifactDir: fixtureDir,
			sitePath: '/tmp/example-site',
			fixtureExecutionRoute: 'host',
			runCli: async (command, context) => {
				calls.push({ command, role: context.role });
				return { exitCode: 0, stdout: `ok:${command}`, stderr: '' };
			},
			setupWordPressFixture: async ({ runCli }) => {
				await runCli('wp option get hook_ready');
				return { hook: true };
			},
			fixtures: [
				{ id: 'seed-posts', type: 'wp-eval-file', path: 'fixtures/seed-posts.php' },
				{ id: 'ready-flag', type: 'wp-cli', command: 'option update homeboy_fixture_ready 1' },
				{
					id: 'already-ready',
					type: 'wp-cli',
					command: 'post create --post_title=Skipped',
					skipIf: 'option get homeboy_fixture_ready',
				},
			],
		});

		assert.equal(result.status, 'passed');
		assert.equal(result.steps.length, 4);
		assert.equal(result.steps[1].command, "eval-file 'fixtures/seed-posts.php'");
		assert.equal(result.steps[3].status, 'skipped');
		assert.equal(calls.some((call) => call.command === 'post create --post_title=Skipped'), false);
		assert.ok(fs.existsSync(result.artifacts.fixtureSetup));
		assert.match(fs.readFileSync(result.artifacts.fixtureSetup, 'utf8'), /seed-posts/);
		assert.deepEqual(
			fixtureRecipeStep({ type: 'wp-cli', command: 'wp option get blogname' }),
			{ command: 'wordpress.wp-cli', args: ['command=option get blogname'] }
		);
		assert.deepEqual(
			fixtureRecipeStep({ type: 'wp-eval-file', path: 'fixtures/seed.php' }),
			{ command: 'wordpress.run-php', args: ['code-file=fixtures/seed.php'] }
		);
		assert.deepEqual(
			wpCodeboxPluginStateStep({ activate: ['source-plugin/source-plugin.php'], deactivate: [{ slug: 'old-plugin' }] }),
			{
				command: 'wordpress.plugin-state',
				args: ['plugin-state-json={"activate":[{"plugin":"source-plugin/source-plugin.php"}],"deactivate":[{"slug":"old-plugin","plugin":"old-plugin"}],"report":true}'],
			}
		);

		const recipeSteps = [];
		const wpCodeboxResult = await runWordPressFixtureSetup({
			fixtureExecutionRoute: 'wp-codebox',
			runRecipeStep: async (recipeStep, context) => {
				recipeSteps.push({ recipeStep, role: context.role });
				if (recipeStep.args[0] === 'command=option get homeboy_fixture_ready') {
					return { exitCode: 1, stdout: '', stderr: 'not ready' };
				}
				return { exitCode: 0, stdout: `ok:${recipeStep.command}`, stderr: '' };
			},
			fixtures: [
				{
					id: 'seed-codebox-posts',
					type: 'wp-eval-file',
					path: 'fixtures/seed-codebox-posts.php',
					skipIf: 'option get homeboy_fixture_ready',
				},
			],
		});
		assert.equal(wpCodeboxResult.status, 'passed');
		assert.equal(wpCodeboxResult.steps[0].status, 'check-failed');
		assert.deepEqual(wpCodeboxResult.steps[1].recipeStep, {
			command: 'wordpress.run-php',
			args: ['code-file=fixtures/seed-codebox-posts.php'],
		});
		assert.deepEqual(recipeSteps.map((call) => call.recipeStep.command), ['wordpress.wp-cli', 'wordpress.run-php']);

		const sitePath = path.join(fixtureDir, 'site');
		const pluginsDir = path.join(sitePath, 'wp-content', 'plugins');
		const sourceDir = path.join(fixtureDir, 'source-plugin');
		const copySourceDir = path.join(fixtureDir, 'copy-source-plugin');
		const existingDir = path.join(pluginsDir, 'source-plugin');
		fs.mkdirSync(sourceDir, { recursive: true });
		fs.mkdirSync(copySourceDir, { recursive: true });
		fs.mkdirSync(existingDir, { recursive: true });
		fs.writeFileSync(path.join(sourceDir, 'source-plugin.php'), '<?php /* Plugin Name: Source Plugin */');
		fs.writeFileSync(path.join(copySourceDir, 'copy-source-plugin.php'), '<?php /* Plugin Name: Copy Source Plugin */');
		fs.writeFileSync(path.join(existingDir, 'existing.txt'), 'existing plugin');

		const activationCalls = [];
		const installedPlugins = await installWordPressFixturePlugins({
			sitePath,
			plugins: [
				{ path: sourceDir, plugin: 'source-plugin/source-plugin.php' },
				{ path: copySourceDir, copy: true, activate: false },
			],
			runCli: async (command, context) => {
				activationCalls.push({ command, slug: context.plugin.slug, timeoutMs: context.timeoutMs });
				return { exitCode: 0, stdout: 'activated', stderr: '' };
			},
			activateTimeoutMs: 12345,
		});
		assert.equal(installedPlugins.length, 2);
		assert.equal(installedPlugins[0].hadExistingPath, true);
		assert.equal(fs.lstatSync(path.join(pluginsDir, 'source-plugin')).isSymbolicLink(), true);
		assert.equal(fs.existsSync(path.join(pluginsDir, 'copy-source-plugin', 'copy-source-plugin.php')), true);
		assert.deepEqual(activationCalls, [{ command: 'plugin activate source-plugin/source-plugin.php', slug: 'source-plugin', timeoutMs: 12345 }]);

		const codeboxActivationCalls = [];
		const codeboxInstalledPlugins = await installWordPressFixturePlugins({
			sitePath,
			fixtureExecutionRoute: 'wp-codebox',
			plugins: [{ path: sourceDir, plugin: 'source-plugin/source-plugin.php' }],
			runRecipeStep: async (recipeStep, context) => {
				codeboxActivationCalls.push({ recipeStep, slug: context.plugin.slug, timeoutMs: context.timeoutMs });
				return { exitCode: 0, stdout: 'activated', stderr: '' };
			},
			activateTimeoutMs: 23456,
		});
		assert.equal(codeboxActivationCalls.length, 1);
		assert.equal(codeboxActivationCalls[0].recipeStep.command, 'wordpress.plugin-state');
		assert.deepEqual(JSON.parse(codeboxActivationCalls[0].recipeStep.args[0].replace(/^plugin-state-json=/, '')), {
			activate: [{ plugin: 'source-plugin/source-plugin.php', slug: 'source-plugin' }],
			deactivate: [],
			report: true,
		});
		assert.equal(codeboxActivationCalls[0].timeoutMs, 23456);
		assert.equal(codeboxInstalledPlugins[0].activation.recipeStep.command, 'wordpress.plugin-state');
		await restoreWordPressFixturePlugins(codeboxInstalledPlugins);

		await restoreWordPressFixturePlugins(installedPlugins);
		assert.equal(fs.readFileSync(path.join(existingDir, 'existing.txt'), 'utf8'), 'existing plugin');
		assert.equal(fs.existsSync(path.join(pluginsDir, 'copy-source-plugin')), false);

		const callbackSource = path.join(fixtureDir, 'callback-plugin');
		fs.mkdirSync(callbackSource, { recursive: true });
		fs.writeFileSync(path.join(callbackSource, 'callback-plugin.php'), '<?php /* Plugin Name: Callback Plugin */');
		await assert.rejects(
			() => withWordPressFixturePlugins(
				{
					sitePath,
					plugins: [{ path: callbackSource, activate: false }],
				},
				async () => {
					assert.equal(fs.existsSync(path.join(pluginsDir, 'callback-plugin')), true);
					throw new Error('callback failure');
				}
			),
			/callback failure/
		);
		assert.equal(fs.existsSync(path.join(pluginsDir, 'callback-plugin')), false);

		await assert.rejects(
			() => runWordPressFixtureSetup({
				runCli: async () => ({ exitCode: 0, stdout: 'implicit host', stderr: '' }),
				fixtures: [{ id: 'implicit-host', type: 'wp-cli', command: 'option get blogname' }],
			}),
			/explicit execution route/
		);

		await assert.rejects(
			() => runWordPressFixtureSetup({
				fixtureExecutionRoute: 'host',
				runCli: async () => ({ exitCode: 2, stdout: 'fixture stdout', stderr: 'fixture stderr' }),
				fixtures: [{ id: 'broken', type: 'wp-cli', command: 'option update broken 1' }],
			}),
			(error) => {
				assert.match(error.message, /WordPress fixture step "broken" failed/);
				assert.match(error.message, /option update broken 1/);
				assert.match(error.message, /fixture stdout/);
				assert.match(error.message, /fixture stderr/);
				assert.equal(error.fixtureSummary.status, 'failed');
				return true;
			}
		);

		console.log('WordPress fixture setup smoke passed.');
	} finally {
		fs.rmSync(fixtureDir, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
