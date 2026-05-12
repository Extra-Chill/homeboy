'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
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

		await assert.rejects(
			() => runWordPressFixtureSetup({
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
