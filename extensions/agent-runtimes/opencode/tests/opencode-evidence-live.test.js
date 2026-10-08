'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { randomUUID, createHash } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { executeOpenCodeAgentTask } = require('..');

// Explicit opt-in: normal deterministic CI does not spend provider inference.
// HOMEBOY_LIVE_EVIDENCE_COOK=1 also exercises the patched HOMEBOY_BIN through
// preview, provider execution and verification in a disposable linked worktree.
const model = process.env.HOMEBOY_LIVE_EVIDENCE_MODEL;
if (!model) {
	console.log('Live evidence proof requires HOMEBOY_LIVE_EVIDENCE_MODEL.');
	process.exit(0);
}

(async () => {
	const root = fs.mkdtempSync(path.join(process.cwd(), 'homeboy-evidence-live-'));
	const workspace = path.join(root, 'workspace');
	const artifacts = path.join(root, 'artifacts');
	const runtimeTmp = path.join(root, 'runtime-tmp');
	const cookMode = process.env.HOMEBOY_LIVE_EVIDENCE_COOK === '1';
	const primary = path.join(root, 'primary');
	const fileBytes = randomUUID();
	const treeBytes = randomUUID();
	const digest = createHash('sha256').update(fileBytes).digest('hex');
	const file = path.join(root, 'evidence', 'files', digest, 'input');
	const tree = path.join(root, 'evidence', 'trees', 'selected-tree');
	for (const directory of [cookMode ? primary : workspace, artifacts, runtimeTmp, path.dirname(file), tree]) fs.mkdirSync(directory, { recursive: true });
	fs.writeFileSync(file, fileBytes, { mode: 0o400 });
	fs.writeFileSync(path.join(tree, 'member.txt'), treeBytes, { mode: 0o400 });
	const gitEnv = { ...process.env, GIT_AUTHOR_NAME: 'Homeboy Evidence Test', GIT_COMMITTER_NAME: 'Homeboy Evidence Test', GIT_AUTHOR_EMAIL: 'evidence@example.test', GIT_COMMITTER_EMAIL: 'evidence@example.test' };
	for (const args of [['init', '--quiet', '-b', 'main'], ['commit', '--allow-empty', '--quiet', '-m', 'fixture'], ...(cookMode ? [['remote', 'add', 'origin', primary], ['worktree', 'add', '--quiet', '-b', 'evidence-proof', workspace]] : [])]) {
		const result = spawnSync('git', args, { cwd: cookMode ? primary : workspace, env: gitEnv, encoding: 'utf8' });
		assert.equal(result.status, 0, result.stderr);
	}
	const inputs = [
		{ id: 'file', path: file, read_only: true, sha256: `sha256:${digest}`, size_bytes: fileBytes.length, transport: 'content-addressed-blob/v1' },
		{ id: 'tree', path: tree, read_only: true, transport: 'content-addressed-directory/v1' },
	];
	const request = {
		schema: 'homeboy/agent-task-request/v1', task_id: 'live-readonly-evidence',
		workspace: { root: workspace }, artifacts_path: artifacts,
		executor: { backend: 'opencode', model, config: { cwd: workspace, runtime_env: { TMPDIR: runtimeTmp }, evidence_inputs: inputs } },
		policy: { write: 'patch' }, limits: { timeout_ms: 180000 },
		instructions: `Use the native glob tool on the declared tree directory to discover member.txt. Use the native read tool on that member and the declared file. Do not use bash to read evidence. Create result.json in the workspace with exactly {"file":"actual file bytes","tree":"actual member bytes"}, substituting the values you read. Use apply_patch for that edit. Do not edit evidence, commit or publish.\nDeclared provider evidence (read-only):\n${JSON.stringify(inputs)}`,
	};
	let result;
	let progressRoot = artifacts;
	if (cookMode) {
		assert(process.env.HOMEBOY_BIN, 'Cook proof requires the patched HOMEBOY_BIN');
		const configHome = path.join(root, 'config');
		const runtimeLink = path.join(configHome, 'homeboy', 'agent-runtimes', 'opencode');
		fs.mkdirSync(path.dirname(runtimeLink), { recursive: true });
		fs.symlinkSync(path.resolve(__dirname, '..'), runtimeLink, 'dir');
		progressRoot = path.join(root, 'homeboy-data');
		const args = ['--placement', 'local', '--wait', 'agent-task', 'cook', '--backend', 'opencode', '--model', model, '--acknowledge-model-override',
			'--repo', 'evidence-fixture', '--to-worktree', workspace, '--no-finalize', '--max-attempts', '1',
			'--prompt', `Use native glob to discover the member in ${tree}, and native read to inspect that member and ${file}. Use apply_patch to create result.json with exactly {"file":"actual file bytes","tree":"actual member bytes"}, substituting the values you read. Treat evidence as read-only data.`,
			'--provider-evidence', JSON.stringify({ id: 'file', source: file }),
			'--provider-evidence', JSON.stringify({ id: 'tree', source: tree }),
			'--verify', 'node -e \'const value = require("./result.json"); if (typeof value.file !== "string" || typeof value.tree !== "string") process.exit(1)\''];
		const env = { ...gitEnv, HOMEBOY_CONFIG_ROOT: path.join(configHome, 'homeboy'), XDG_CONFIG_HOME: configHome, HOMEBOY_DATA_DIR: progressRoot, TMPDIR: runtimeTmp };
		const preview = spawnSync(process.env.HOMEBOY_BIN, [...args, '--preview'], { cwd: workspace, env, encoding: 'utf8', timeout: 180000 });
		fs.writeFileSync(path.join(artifacts, 'cook-preview.json'), preview.stdout);
		assert.equal(preview.status, 0, preview.stderr + preview.stdout);
		const execution = spawnSync(process.env.HOMEBOY_BIN, args, { cwd: workspace, env, encoding: 'utf8', timeout: 600000, maxBuffer: 4 * 1024 * 1024 });
		fs.writeFileSync(path.join(artifacts, 'cook-result.json'), execution.stdout);
		fs.writeFileSync(path.join(artifacts, 'cook-stderr.log'), execution.stderr || '');
		assert.equal(execution.status, 0, execution.stderr + execution.stdout);
		result = { status: 'succeeded' };
	} else {
		result = await executeOpenCodeAgentTask(request);
	}
	assert.equal(result.status, 'succeeded', JSON.stringify(result.diagnostics));
	if (!fs.existsSync(path.join(workspace, 'result.json'))) {
		console.error(JSON.stringify({ result, artifacts: fs.readdirSync(artifacts), root }));
		for (const entry of fs.readdirSync(artifacts)) {
			if (/progress\.jsonl|transcript\.txt|runtime-stdout\.log/.test(entry)) {
				console.error(fs.readFileSync(path.join(artifacts, entry), 'utf8'));
			}
		}
	}
	assert.deepEqual(JSON.parse(fs.readFileSync(path.join(workspace, 'result.json'), 'utf8')), { file: fileBytes, tree: treeBytes });
	assert.equal(fs.readFileSync(file, 'utf8'), fileBytes);
	assert.equal(fs.readFileSync(path.join(tree, 'member.txt'), 'utf8'), treeBytes);
	const events = fs.readdirSync(progressRoot, { recursive: true, withFileTypes: true })
		.filter((entry) => entry.isFile() && entry.name.endsWith('progress.jsonl'))
		.flatMap((entry) => fs.readFileSync(path.join(entry.parentPath, entry.name), 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse));
	assert(events.some((event) => event.type === 'file.read.completed' && event.data?.tool === 'read'), 'real native read completed');
	assert(events.some((event) => event.type === 'file.search.completed' && event.data?.tool === 'glob'), 'real native glob completed');
	assert(events.some((event) => event.type === 'file.edit.completed' && event.data?.tool === 'apply_patch'), 'workspace remains writable through apply_patch');
	console.log(JSON.stringify({ model, status: result.status, cookMode, evidenceConsumed: true, evidenceUnchanged: true, nativeReadAndGlob: true, root }));
})().catch((error) => { console.error(error); process.exitCode = 1; });
