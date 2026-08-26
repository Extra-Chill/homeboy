'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const runtimeRoot = path.join(__dirname, '..');
const executor = path.join(runtimeRoot, 'scripts', 'agent', 'homeboy-opencode-agent-task-executor.cjs');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-progress-relay-'));

function run(request) {
	const child = spawn(process.execPath, [executor], {
		cwd: root,
		env: { ...process.env, HOMEBOY_AGENT_TASK_REQUEST: JSON.stringify(request) },
		stdio: ['ignore', 'pipe', 'pipe'],
	});
	const stdout = [];
	const stderr = [];
	let exited = false;
	let progressBeforeExit = false;
	child.stdout.on('data', (chunk) => stdout.push(chunk));
	child.stderr.on('data', (chunk) => {
		stderr.push(chunk);
		progressBeforeExit ||= !exited;
	});
	return {
		interim: () => ({ stdout: Buffer.concat(stdout).toString('utf8'), stderr: Buffer.concat(stderr).toString('utf8') }),
		result: new Promise((resolve, reject) => {
			child.on('error', reject);
			child.on('close', (status) => resolve({ status, stdout: Buffer.concat(stdout).toString('utf8'), stderr: Buffer.concat(stderr).toString('utf8'), progressBeforeExit }));
			child.on('exit', () => { exited = true; });
		}),
	};
}

function requestFor(commandArgs, artifactsPath) {
	return {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'relay-fixture',
		instructions: 'Verify progress relay.',
		artifacts_path: artifactsPath,
		executor: { backend: 'opencode', runtime: 'opencode', config: { runtime_bin: process.execPath, command_args: commandArgs } },
	};
}

(async () => {
	try {
		const streamingCli = path.join(root, 'streaming-opencode.cjs');
		fs.writeFileSync(streamingCli, `#!/usr/bin/env node
process.stdout.write(JSON.stringify({ type: 'tool_use', sessionID: 'ses_fixture_123', timestamp: 1787763877191, part: { type: 'tool', tool: 'bash', state: { status: 'completed', input: { command: 'TOKEN=super-secret-token run --snapshot=' + 'x'.repeat(10000) } } } }) + '\\n');
setTimeout(() => process.exit(0), 250);
`);
		const streamed = run(requestFor([streamingCli], path.join(root, 'stream-artifacts')));
		const streamedResult = await streamed.result;
		assert.equal(streamedResult.status, 0, streamedResult.stderr);
		assert.equal(streamedResult.progressBeforeExit, true);
		assert.equal(streamedResult.stdout.includes('super-secret-token'), false);
		assert.equal(streamedResult.stderr.includes('super-secret-token'), false);
		assert.equal(streamedResult.stderr.includes('snapshot='), false);
		const relayed = streamedResult.stderr.trim().split('\n').map((line) => JSON.parse(line));
		assert.deepEqual(relayed, [{
			schema: 'homeboy/agent-task-runtime-progress/v1', provider: 'opencode', session_id: 'ses_fixture_123',
			category: 'command.completed', latest_activity_at: '2026-08-26T17:04:37.191Z',
		}]);
		assert.ok(Buffer.byteLength(streamedResult.stderr) < 300);
		const runtimeLog = fs.readFileSync(path.join(root, 'stream-artifacts', 'relay-fixture-opencode-runtime-stdout.log'), 'utf8');
		assert.match(runtimeLog, /snapshot=/);

		const silentCli = path.join(root, 'silent-opencode.cjs');
		fs.writeFileSync(silentCli, '#!/usr/bin/env node\nsetTimeout(() => process.exit(0), 250);\n');
		const silent = run(requestFor([silentCli], path.join(root, 'silent-artifacts')));
		await new Promise((resolve) => setTimeout(resolve, 75));
		assert.deepEqual(silent.interim(), { stdout: '', stderr: '' });
		const silentResult = await silent.result;
		assert.equal(silentResult.status, 0, silentResult.stderr);
		assert.equal(silentResult.stderr, '');
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
})().then(() => process.stdout.write('OpenCode parent progress relay passed\n'));
