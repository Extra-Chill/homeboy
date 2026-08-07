'use strict';

require('../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { createCliAgentTaskExecutor } = require('./cli-agent-task-executor');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-cli-executor-conformance-'));
try {
	const command = path.join(root, 'fixture-command.cjs');
	fs.writeFileSync(command, `#!/usr/bin/env node
const mode = process.argv[2];
if (mode === 'success') process.stdout.write('completed');
if (mode === 'failure') { process.stdout.write('partial'); process.stderr.write('failed'); process.exit(23); }
if (mode === 'cancelled') process.kill(process.pid, 'SIGTERM');
`);

	const executor = createCliAgentTaskExecutor({
		backend: 'fixture',
		providerId: 'fixture.agent-task-executor',
		providerLabel: 'Fixture agent',
		defaultSummary: 'Fixture executor failed.',
		artifactProvider: 'fixture',
		collectArtifacts: true,
		resolveCommandSpec: () => ({ command: process.execPath, args: [command] }),
		buildArgs: (request, config, commandSpec) => [...commandSpec.args, config.mode],
		buildSpawn: () => ({}),
		messages: {
			invalidRequest: { code: 'invalid_request', summary: 'Invalid request.' },
			invalidCommand: { code: 'invalid_command', summary: 'Invalid command.' },
			notFound: { code: 'not_found', summary: 'Not found.', hint: 'Install fixture.' },
			timeout: { code: 'timeout', summary: 'Timed out.' },
			spawnFailed: { code: 'spawn_failed', summary: 'Spawn failed.' },
			success: { summary: 'Succeeded.', diag: 'Completed.' },
			failed: { code: 'failed', summary: 'Failed.', diag: (status) => `Exited ${status}.` },
		},
	});

	for (const [mode, expectedStatus] of [['success', 'succeeded'], ['failure', 'failed'], ['cancelled', 'failed'], ['no-candidate', 'succeeded']]) {
		const outcome = executor.execute({
			schema: 'homeboy/agent-task-request/v1',
			task_id: `fixture-${mode}`,
			executor: {
				backend: 'fixture',
				config: { mode: mode === 'no-candidate' ? 'success' : mode, artifacts_path: path.join(root, mode) },
			},
			instructions: 'Run the fixture.',
			...(mode === 'no-candidate' ? { artifact_declarations: [{ name: 'patch', required: true }, { name: 'agent_result', required: true }, { name: 'transcript', required: true }] } : {}),
		});
		assert.equal(outcome.status, expectedStatus);
		assert.equal(outcome.evidence_refs.every((ref) => typeof ref.uri === 'string' && ref.uri.startsWith('file:') && !Object.hasOwn(ref, 'path')), true);
		assert.equal(outcome.artifacts.every((artifact) => artifact.schema === 'homeboy/agent-task-artifact/v1' && typeof artifact.uri === 'string' && artifact.uri.startsWith('file:') && Number.isInteger(artifact.size_bytes)), true);
		assert.equal(outcome.artifacts.every((artifact) => artifact.url === artifact.uri), true);
		assert.equal(outcome.artifacts.every((artifact) => artifact.bytes === artifact.size_bytes), true);
		if (mode === 'cancelled') {
			assert.equal(outcome.metadata.signal, 'SIGTERM');
			assert.equal(outcome.metadata.exit_code, null);
		}
		if (mode === 'no-candidate') {
			assert.deepEqual(outcome.artifacts.map((artifact) => artifact.name), ['fixture-stdout']);
			assert.equal(outcome.diagnostics.some((diagnostic) => diagnostic.class === 'agent_task.required_declared_artifact_missing'), false);
		}
		assertStrictAgentTaskOutcome(outcome);
	}
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('CLI agent task executor cross-runtime conformance passed\n');

function assertStrictAgentTaskOutcome(outcome) {
	assert.equal(outcome.schema, 'homeboy/agent-task-outcome/v1');
	assert.equal(typeof outcome.task_id, 'string');
	assert.equal(typeof outcome.status, 'string');
	assert.equal(typeof outcome.summary, 'string');
	assert.equal(Array.isArray(outcome.artifacts), true);
	assert.equal(Array.isArray(outcome.evidence_refs), true);
	for (const artifact of outcome.artifacts) {
		assert.equal(artifact.schema, 'homeboy/agent-task-artifact/v1');
		assert.equal(typeof artifact.id, 'string');
		assert.equal(typeof artifact.kind, 'string');
		assert.equal(typeof artifact.uri, 'string');
		assert.equal(new URL(artifact.uri).protocol, 'file:');
		assert.equal(Number.isInteger(artifact.size_bytes), true);
	}
	for (const ref of outcome.evidence_refs) {
		assert.equal(typeof ref.kind, 'string');
		assert.equal(typeof ref.uri, 'string');
		assert.equal(new URL(ref.uri).protocol, 'file:');
	}
}
