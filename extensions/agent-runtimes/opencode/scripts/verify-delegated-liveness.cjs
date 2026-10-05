#!/usr/bin/env node
'use strict';

// Explicit live check; deliberately outside the automatically selected tests.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { executeOpenCodeAgentTask } = require('../lib/opencode-agent-task-executor');

async function main() {
	const model = process.env.HOMEBOY_TEST_MODEL;
	assert.ok(model, 'Set HOMEBOY_TEST_MODEL to the verified live provider/model');
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-delegation-live-'));
	const workspace = path.join(root, 'workspace');
	const artifacts = path.join(root, 'artifacts');
	fs.mkdirSync(workspace);
	fs.mkdirSync(artifacts);
	fs.writeFileSync(path.join(workspace, 'probe.txt'), 'delegation activity probe\n');
	assert.equal(spawnSync('git', ['init', '-q', workspace]).status, 0);
	assert.equal(spawnSync('git', ['-C', workspace, 'add', 'probe.txt']).status, 0);
	const gitEnv = { ...process.env, GIT_AUTHOR_NAME: 'Runtime fixture', GIT_AUTHOR_EMAIL: 'fixture@example.test', GIT_COMMITTER_NAME: 'Runtime fixture', GIT_COMMITTER_EMAIL: 'fixture@example.test' };
	assert.equal(spawnSync('git', ['-C', workspace, 'commit', '-qm', 'runtime fixture'], { env: gitEnv }).status, 0);
	try {
		const result = await executeOpenCodeAgentTask({
			schema: 'homeboy/agent-task-request/v1',
			task_id: 'delegation-live-probe',
			executor: { backend: 'opencode', model, config: { artifacts_path: artifacts, timeout_seconds: 300 } },
			workspace: { mode: 'existing', root: workspace },
			instructions: 'Use the task tool to delegate to a general subagent. Ask it to complete twelve sequential rounds: in EACH round first use bash to sleep 5 seconds, then use the read tool to read probe.txt. Each round must be separate tool invocations; run rounds sequentially, with no edits. Wait for the delegated task to finish, then reply DELEGATION_COMPLETE. This is an observation-only runtime test, not a coding task.',
			limits: { timeout_ms: 300000 },
		}, { env: process.env });
		assert.ok(['succeeded', 'no_op'].includes(result.status), JSON.stringify(result));
		const activityPath = path.join(artifacts, 'delegation-live-probe-opencode-runtime-session-activity.jsonl');
		const activity = fs.readFileSync(activityPath, 'utf8').trim().split('\n').map(JSON.parse);
		const rootId = JSON.parse(fs.readFileSync(`${activityPath}.root.json`, 'utf8')).session_id;
		const descendantEvents = activity.filter((event) => event.session_id !== rootId);
		assert.ok(descendantEvents.length >= 12, 'native delegated tool activity must be observed');
		const duration = Date.parse(descendantEvents.at(-1).timestamp) - Date.parse(descendantEvents[0].timestamp);
		assert.ok(duration >= 50000, 'the delegated parent must stay busy through a substantial quiet interval');
		const stream = fs.readFileSync(path.join(artifacts, 'delegation-live-probe-opencode-runtime-stdout.log'), 'utf8').trim().split('\n').map(JSON.parse);
		assert.ok(stream.every((event) => !event.sessionID || event.sessionID === rootId), 'CLI still only streams the root; observer must supply descendant progress');
		const evidence = { status: result.status, rootSession: rootId, descendantEvents: descendantEvents.length, delegatedIntervalMs: duration, activitySchema: activity[0].schema, artifacts: result.artifacts?.map((artifact) => artifact.name) };
		if (process.env.HOMEBOY_LIVE_EVIDENCE_FILE) {
			fs.mkdirSync(path.dirname(process.env.HOMEBOY_LIVE_EVIDENCE_FILE), { recursive: true });
			fs.writeFileSync(process.env.HOMEBOY_LIVE_EVIDENCE_FILE, JSON.stringify(evidence, null, 2));
		}
		console.log(JSON.stringify(evidence, null, 2));
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

main().catch((error) => { console.error(error); process.exitCode = 1; });
