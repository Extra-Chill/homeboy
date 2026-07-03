#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { runHeadlessDeterministicLoop } = require('../runtime-agent-ci/lib/headless-deterministic-loop-runner.js');

const runnerSource = fs.readFileSync(path.join(repoRoot, 'runtime-agent-ci/lib/headless-deterministic-loop-runner.js'), 'utf8');
assert.doesNotMatch(runnerSource, /HOMEBOY_WP_CODEBOX|WP_CODEBOX/);

const tempDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-controller-env-')));
const homeboyBin = path.join(tempDir, 'homeboy-controller-stub.cjs');
fs.writeFileSync(homeboyBin, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const outputIndex = args.indexOf('--output');
if (outputIndex !== -1) {
  fs.writeFileSync(path.resolve(process.cwd(), args[outputIndex + 1]), JSON.stringify({ status: 'succeeded', args }, null, 2) + '\\n');
}
`, { mode: 0o755 });

const previousRuntime = process.env.RUNTIME;
const previousBin = process.env.HOMEBOY_WP_CODEBOX_BIN;
const previousCore = process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;

try {
	process.env.RUNTIME = 'wp-codebox';
	process.env.HOMEBOY_WP_CODEBOX_BIN = '/bin/echo';
	process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE = '/tmp/wp-codebox-core.js';

	const result = await runHeadlessDeterministicLoop({
		repoRoot,
		workspace: tempDir,
		homeboyBin,
		spec: {
			loop_id: 'controller-env-boundary',
			runtime: 'wp-codebox',
			component_path: tempDir,
			task_id: 'controller-env-boundary-task',
			controller_execution: {
				spec: 'controller-spec.json',
				output: 'controller-output.json',
			},
		},
	});

	assert.equal(result.status, 'succeeded');
	const args = result.tasks[0].outcome.metadata.controller_result.args;
	assert.ok(args.includes('--runner-env'));
	assert.ok(args.includes('HOMEBOY_WP_CODEBOX_BIN=/bin/echo'));
	assert.ok(args.includes('HOMEBOY_WP_CODEBOX_CORE_MODULE=/tmp/wp-codebox-core.js'));
	assert.equal(args.some((arg) => arg.startsWith('HOMEBOY_AGENT_RUNTIME_PROVIDER=')), false);
} finally {
	restoreEnv('RUNTIME', previousRuntime);
	restoreEnv('HOMEBOY_WP_CODEBOX_BIN', previousBin);
	restoreEnv('HOMEBOY_WP_CODEBOX_CORE_MODULE', previousCore);
}

console.log('headless controller runner env boundary passed');

function restoreEnv(name, value) {
	if (value === undefined) {
		delete process.env[name];
		return;
	}
	process.env[name] = value;
}
