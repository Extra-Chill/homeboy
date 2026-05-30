'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	fixtureRecipeStep,
	normalizeFixtureList,
	runWordPressFixtureSetup,
} = require('../lib/fixture-setup');

const fixtureDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-fixtures-'));

async function main() {
	try {
		assert.deepEqual(
			normalizeFixtureList([{ path: 'fixtures/seed.php' }]).map((step) => ({ type: step.type, label: step.label })),
			[{ type: 'wp-eval-file', label: 'wp-eval-file:1' }]
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

		await assert.rejects(
			() => runWordPressFixtureSetup({
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
