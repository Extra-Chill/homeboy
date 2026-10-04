'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	MAX_INLINE_INSTRUCTIONS_BYTES,
	executePiAgentTask,
	parsePiEventStream,
	providerContract,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');

// ── Contract ────────────────────────────────────────────────────────
const provider = providerContract();
assert.equal(provider.id, 'pi.agent-task-executor');
assert.equal(provider.backend, 'pi');
assert.equal(provider.runtime, 'pi');
assert.equal(provider.status, 'experimental');
assert.equal(provider.integration_contract, 'homeboy-pi-agent-task/v1');
assert.equal(Object.hasOwn(provider.lifecycle, 'max_concurrency_default'), false);
assert.equal(provider.lifecycle.cancellation, 'process_signal');
assert.deepEqual(provider.secret_env_requirements, []);
assert.deepEqual(provider.provider_defaults, { pi: { command: 'pi', command_args: ['--mode', 'json'] } });
assert.equal(provider.capabilities.includes('cli_runtime'), true);
assert.equal(provider.capabilities.includes('structured_outcome'), true);
assert.equal(provider.capabilities.includes('provider_owned_auth'), true);
// Pi has no MCP client, so it does not claim runtime tool attachment.
assert.equal(provider.capabilities.includes('runtime_tool_attachment'), false);
assert.equal(provider.readiness_invocation.argv[1], '{{runtime_path}}/scripts/agent/homeboy-pi-provider-readiness.cjs');

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'pi.json'), 'utf8'));
assert.equal(manifest.id, 'pi');
assert.equal(manifest.name, 'Pi');
assert.equal(manifest.agent_task_executors.length, 1);
assert.deepEqual(manifest.agent_task_executors[0], provider);

// ── Event stream parsing ────────────────────────────────────────────
function assistant(overrides = {}) {
	return {
		role: 'assistant',
		content: [{ type: 'text', text: 'Fixed the bug.\n\nDetails follow.' }],
		provider: 'anthropic',
		model: 'claude-test',
		stopReason: 'stop',
		usage: { input: 100, output: 20, cacheRead: 50, cacheWrite: 10, totalTokens: 180, cost: { total: 0.01 } },
		timestamp: 1,
		...overrides,
	};
}

function stream(events) {
	return `${events.map((event) => JSON.stringify(event)).join('\n')}\n`;
}

const okStream = stream([
	{ type: 'session', version: 3, id: 's', cwd: '/w' },
	{ type: 'agent_start' },
	{ type: 'turn_start' },
	{ type: 'message_end', message: assistant({ stopReason: 'toolUse', content: [{ type: 'toolCall', id: 'c1', name: 'bash', arguments: {} }] }) },
	{ type: 'tool_execution_end', toolCallId: 'c1', toolName: 'bash', result: {}, isError: false },
	{ type: 'turn_end', message: {}, toolResults: [] },
	{ type: 'turn_start' },
	{ type: 'message_end', message: assistant() },
	{ type: 'turn_end', message: {}, toolResults: [] },
	{ type: 'agent_end', messages: [], willRetry: false },
	{ type: 'agent_settled' },
]);

const parsed = parsePiEventStream(`${okStream}not json\n\r\n`);
assert.equal(parsed.turns, 2);
assert.equal(parsed.toolCalls, 1);
assert.equal(parsed.settled, true);
assert.equal(parsed.invalidLines, 1);
assert.equal(parsed.finalAssistant.stopReason, 'stop');
assert.equal(parsed.finalText, 'Fixed the bug.\n\nDetails follow.');
assert.deepEqual(parsed.usage, { input: 200, output: 40, cacheRead: 100, cacheWrite: 20, totalTokens: 360, cost: 0.02 });
// Unicode line separators inside JSON strings are not record boundaries.
assert.equal(parsePiEventStream(stream([{ type: 'message_end', message: assistant({ content: [{ type: 'text', text: 'a\u2028b' }] }) }])).finalText, 'a\u2028b');

// ── Execution against a fake pi binary ──────────────────────────────
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-pi-runtime-'));
try {
	const workspace = path.join(root, 'workspace');
	const artifacts = path.join(root, 'artifacts');
	fs.mkdirSync(workspace);
	fs.mkdirSync(artifacts);

	// Records its argv, cwd, stdin and env, then replays a JSONL stream from FAKE_PI_STREAM.
	const fakePi = path.join(root, 'fake-pi.cjs');
	fs.writeFileSync(fakePi, `#!/usr/bin/env node
const fs = require('node:fs');
const stdin = fs.readFileSync(0, 'utf8');
fs.writeFileSync(process.env.FAKE_PI_RECORD, JSON.stringify({
  argv: process.argv.slice(2),
  cwd: process.cwd(),
  stdin,
  undeclaredSecret: process.env.UNDECLARED_SECRET || null,
  agentDir: process.env.PI_CODING_AGENT_DIR || null,
}));
process.stdout.write(fs.readFileSync(process.env.FAKE_PI_STREAM, 'utf8'));
process.stderr.write(process.env.FAKE_PI_STDERR || '');
process.exit(Number(process.env.FAKE_PI_EXIT || 0));
`);

	function run({ events, exit = 0, stderr = '', instructions = 'Fix the failing test.', config = {}, request = {} }) {
		const streamFile = path.join(root, 'stream.jsonl');
		const recordFile = path.join(root, 'record.json');
		fs.writeFileSync(streamFile, typeof events === 'string' ? events : stream(events));
		const result = executePiAgentTask({
			schema: 'homeboy/agent-task-request/v1',
			task_id: 'pi-task',
			workspace_path: workspace,
			artifacts_path: artifacts,
			executor: {
				backend: 'pi',
				runtime: 'pi',
				config: {
					command: process.execPath,
					command_args: [fakePi, '--mode', 'json'],
					runtime_env: {
						FAKE_PI_STREAM: streamFile,
						FAKE_PI_RECORD: recordFile,
						FAKE_PI_EXIT: String(exit),
						FAKE_PI_STDERR: stderr,
					},
					...config,
				},
			},
			instructions,
			...request,
		}, { env: { ...process.env, UNDECLARED_SECRET: 'must-not-reach-pi', PI_CODING_AGENT_DIR: '/agent-dir' } });
		return { result, record: JSON.parse(fs.readFileSync(recordFile, 'utf8')) };
	}

	// Success: argv shape, workspace cwd, empty stdin, env isolation, outcome and metadata.
	{
		const { result, record } = run({ events: okStream, config: { model: 'anthropic/claude-test' } });
		assert.equal(result.status, 'succeeded', JSON.stringify(result));
		assert.equal(result.summary, 'Fixed the bug.');
		assert.deepEqual(record.argv, ['--mode', 'json', '--session-dir', path.join(artifacts, 'pi-session'), '--model', 'anthropic/claude-test', 'Fix the failing test.']);
		assert.equal(fs.realpathSync(record.cwd), fs.realpathSync(workspace));
		assert.equal(record.stdin, '');
		assert.equal(record.undeclaredSecret, null);
		assert.equal(record.agentDir, '/agent-dir');
		assert.equal(result.metadata.stop_reason, 'stop');
		assert.equal(result.metadata.model, 'claude-test');
		assert.equal(result.metadata.turns, 2);
		assert.equal(result.metadata.tool_calls, 1);
		assert.equal(result.metadata.usage.total_tokens, 360);
		assert.equal(result.metadata.usage.cost_usd, 0.02);
		assert.equal(result.artifacts.some((artifact) => artifact.name === 'pi-stdout'), true);
	}

	// No artifacts dir: no session file is written.
	{
		const { record } = run({ events: okStream, request: { artifacts_path: undefined } });
		assert.equal(record.argv.includes('--no-session'), true);
	}

	// The model's own error on a clean exit is a provider error, not success.
	{
		const { result } = run({ events: [{ type: 'message_end', message: assistant({ stopReason: 'error', errorMessage: '401 invalid x-api-key' }) }] });
		assert.equal(result.status, 'provider_error');
		assert.equal(result.failure_code, 'agent_task.pi_model_error');
		assert.match(result.summary, /401 invalid x-api-key/);
	}

	// Retries exhausted.
	{
		const { result } = run({ events: [
			{ type: 'message_end', message: assistant() },
			{ type: 'auto_retry_end', success: false, attempt: 3, finalError: '529 overloaded' },
		] });
		assert.equal(result.status, 'provider_error');
		assert.match(result.summary, /529 overloaded/);
	}

	// Output limit.
	{
		const { result } = run({ events: [{ type: 'message_end', message: assistant({ stopReason: 'length' }) }] });
		assert.equal(result.status, 'failed');
		assert.equal(result.failure_code, 'agent_task.pi_output_limit');
	}

	// Clean exit with no events: misconfigured args, reported as such.
	{
		const { result } = run({ events: '' });
		assert.equal(result.status, 'provider_error');
		assert.equal(result.failure_code, 'agent_task.pi_no_result');
		assert.match(result.diagnostics[0].message, /--mode json/);
	}

	// Nonzero exit carries stderr.
	{
		const { result } = run({ events: '', exit: 2, stderr: 'No API key found for anthropic' });
		assert.equal(result.status, 'failed');
		assert.equal(result.failure_code, 'agent_task.pi_failed');
		assert.equal(result.metadata.exit_code, 2);
		assert.match(result.summary, /No API key found/);
	}

	// Instructions too long for one argv entry go through a file in the artifacts dir.
	{
		const long = 'x'.repeat(MAX_INLINE_INSTRUCTIONS_BYTES + 1);
		const { record } = run({ events: okStream, instructions: long });
		const prompt = record.argv.at(-1);
		const file = path.join(artifacts, 'pi-task-pi-instructions.md');
		assert.match(prompt, /full task instructions are in/);
		assert.equal(prompt.includes(file), true);
		assert.equal(fs.readFileSync(file, 'utf8'), long);
	}

	// Missing binary.
	{
		const result = executePiAgentTask({
			schema: 'homeboy/agent-task-request/v1',
			task_id: 'pi-missing',
			executor: { backend: 'pi', runtime: 'pi', config: { command: path.join(root, 'no-such-pi') } },
			instructions: 'x',
		});
		assert.equal(result.status, 'provider_error');
		assert.equal(result.failure_code, 'agent_task.pi_command_not_found');
	}

	// Invalid request.
	{
		const invalid = executePiAgentTask({ schema: 'wrong', task_id: 't', executor: { backend: 'pi' }, instructions: 'x' });
		assert.equal(invalid.status, 'provider_error');
		assert.equal(invalid.failure_code, 'agent_task.invalid_pi_request');
	}

	// The bin script prints the manifest contract and runs requests from stdin.
	{
		const runtimesRoot = path.join(root, 'agent-runtimes');
		const runtimePath = path.join(runtimesRoot, 'pi');
		fs.mkdirSync(runtimesRoot, { recursive: true });
		fs.symlinkSync(runtimeRoot, runtimePath, 'dir');
		const scriptPath = provider.invocation.argv[1].replaceAll('{{runtime_path}}', runtimePath);
		const contractResult = spawnSync(process.execPath, [scriptPath, '--provider-contract'], { encoding: 'utf8' });
		assert.equal(contractResult.status, 0, contractResult.stderr);
		assert.deepEqual(JSON.parse(contractResult.stdout), manifest.agent_task_executors[0]);
	}
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Pi agent task executor boundary passed\n');
