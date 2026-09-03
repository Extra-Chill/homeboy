#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const canonicalProviderContractPath = path.join(rootDir, 'agent-task-contracts', 'agent-task-provider-contract.js');
const canonicalRunnerContractPath = path.join(rootDir, 'agent-task-contracts', 'agent-task-runner-contract.js');
const {
	AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
	AGENT_TASK_ARTIFACT_FIELDS,
	AGENT_TASK_ARTIFACT_SCHEMA,
	AGENT_TASK_EVIDENCE_REF_FIELDS,
	AGENT_TASK_EVIDENCE_REF_SCHEMA,
	AGENT_TASK_REDACTED_METADATA_KEYS,
	AGENT_TASK_SECRET_SELECTOR_PATHS,
	AGENT_TASK_CAPABILITY_BUNDLES,
	AGENT_TASK_TOOL_PRESETS,
	agentTaskArtifactFromRef,
	agentTaskEvidenceRefFromRef,
	agentTaskProviderContractFields,
	expandAgentTaskCapabilityBundles,
	expandAgentTaskToolPresets,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require(canonicalProviderContractPath);
const {
	AGENT_TASK_RUNNER_SPEC_SCHEMA,
	agentTaskRequestFromRunnerSpec,
	agentTaskRunnerSpec,
	validateAgentTaskRunnerSpec,
} = require(canonicalRunnerContractPath);

assert.equal(AGENT_TASK_ARTIFACT_SCHEMA, 'homeboy/agent-task-artifact/v1');
assert.equal(AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA, 'homeboy/agent-task-artifact-declaration/v1');
assert.equal(AGENT_TASK_EVIDENCE_REF_SCHEMA, 'homeboy/agent-task-evidence-ref/v1');
assert.deepEqual(AGENT_TASK_ARTIFACT_FIELDS, ['schema', 'id', 'kind', 'name', 'path', 'url', 'mime', 'size_bytes', 'sha256', 'metadata']);
assert.deepEqual(AGENT_TASK_EVIDENCE_REF_FIELDS, ['kind', 'uri', 'label']);

const fields = agentTaskProviderContractFields();
assert.equal(fields.request_schema, 'homeboy/agent-task-request/v1');
assert.equal(fields.outcome_schema, 'homeboy/agent-task-outcome/v1');
assert.deepEqual(fields.redacted_metadata_keys, AGENT_TASK_REDACTED_METADATA_KEYS);

const coreContract = JSON.parse(fs.readFileSync(
	path.join(rootDir, 'agent-runtimes', 'fixtures', 'homeboy-agent-task-core-contract.json'),
	'utf8'
));

for (const runtimeId of ['claude-code', 'codex', 'local-shell', 'opencode', 'pi']) {
	const runtimePath = path.join(rootDir, 'agent-runtimes', runtimeId);
	const manifest = JSON.parse(fs.readFileSync(path.join(runtimePath, `${runtimeId}.json`), 'utf8'));
	const [program, scriptTemplate] = manifest.agent_task_executors[0].invocation.argv;
	const result = spawnSync(program, [scriptTemplate.replaceAll('{{runtime_path}}', runtimePath), '--provider-contract'], {
		cwd: rootDir,
		encoding: 'utf8',
		stdio: ['ignore', 'pipe', 'pipe'],
	});
	assert.equal(result.status, 0, `${runtimeId}: ${result.stderr}`);
	assert.deepEqual(JSON.parse(result.stdout), manifest.agent_task_executors[0]);
}
assert.deepEqual(fields, {
	request_schema: coreContract.provider_capability.request_schema,
	outcome_schema: coreContract.provider_capability.outcome_schema,
	request_required_fields: coreContract.provider_capability.request_required_fields,
	outcome_statuses: coreContract.provider_capability.outcome_statuses,
	failure_classifications: coreContract.provider_capability.failure_classifications,
	redacted_metadata_keys: coreContract.provider_capability.redacted_metadata_keys,
});

const extendedRedactionKeys = extendRedactedMetadataKeys('secrets', ['provider_auth', 'codex_auth']);
assert.deepEqual(extendedRedactionKeys, ['secret_env_values', 'secretEnvValues', 'secrets', 'provider_auth', 'codex_auth']);
assert.deepEqual(AGENT_TASK_REDACTED_METADATA_KEYS, ['secret_env_values', 'secretEnvValues', 'secrets']);

const secretRequirement = providerSecretEnvRequirement('codex', ['CODEX_TOKEN']);
assert.equal(secretRequirement.schema, 'homeboy/secret-env-requirement/v1');
assert.deepEqual(secretRequirement.env, ['CODEX_TOKEN']);
assert.deepEqual(secretRequirement.when.any, AGENT_TASK_SECRET_SELECTOR_PATHS.map((selectorPath) => ({
	path: selectorPath,
	equals: 'codex',
})));

const artifact = agentTaskArtifactFromRef({
	type: 'provider-log',
	name: 'Provider log',
	directory: 'artifacts/logs',
	url: 'https://example.test/logs',
	mime: 'text/plain',
	size_bytes: 123,
	sha256: 'abc123',
	metadata: { secret_env_values: { TOKEN: 'redacted' }, public: true },
}, 2, (metadata) => ({ public: metadata.public }));
assert.deepEqual(artifact, {
	schema: AGENT_TASK_ARTIFACT_SCHEMA,
	id: 'abc123',
	kind: 'provider-log',
	name: 'Provider log',
	path: 'artifacts/logs',
	url: 'https://example.test/logs',
	mime: 'text/plain',
	size_bytes: 123,
	sha256: 'abc123',
	metadata: { public: true },
});

assert.deepEqual(agentTaskEvidenceRefFromRef({ type: 'preview', path: 'artifacts/preview.html', name: 'Preview' }), {
	kind: 'preview',
	uri: 'artifacts/preview.html',
	label: 'Preview',
});

assert.deepEqual(Object.keys(AGENT_TASK_TOOL_PRESETS), ['runner_workspace', 'publication']);
assert.deepEqual(expandAgentTaskToolPresets(['runner_workspace', 'publication']), {
	workspace_tools: {
		readonly: ['workspace_ls', 'workspace_read', 'workspace_git_status'],
		readwrite: ['workspace_run', 'workspace_write', 'workspace_edit', 'workspace_apply_patch', 'workspace_delete', 'workspace_git_add'],
	},
	publication_tools: ['publication_prepare', 'publication_publish', 'publication_status'],
});
assert.deepEqual(expandAgentTaskToolPresets(['runner_workspace'], {
	workspace_tools: { readonly: ['workspace_read', 'workspace_stat'] },
	publication_tools: ['publication_status'],
}), {
	workspace_tools: {
		readonly: ['workspace_ls', 'workspace_read', 'workspace_git_status', 'workspace_stat'],
		readwrite: ['workspace_run', 'workspace_write', 'workspace_edit', 'workspace_apply_patch', 'workspace_delete', 'workspace_git_add'],
	},
	publication_tools: ['publication_status'],
});
assert.throws(
	() => expandAgentTaskToolPresets(['product_workspace']),
	/Unknown agent task tool preset: product_workspace/,
);

assert.deepEqual(Object.keys(AGENT_TASK_CAPABILITY_BUNDLES), ['workspace_readwrite', 'github_publication', 'worktree_pr_iteration']);
assert.deepEqual(expandAgentTaskCapabilityBundles(['workspace_readwrite']), {
	tool_presets: ['runner_workspace'],
	provider_runtime_invocation: {
		operations: {
			workspaceCommand: true,
			workspaceCapture: true,
		},
	},
});
assert.deepEqual(expandAgentTaskCapabilityBundles(['worktree_pr_iteration']), {
	tool_presets: ['runner_workspace', 'publication'],
	provider_runtime_invocation: {
		operations: {
			workspaceCommand: true,
			workspaceCapture: true,
			workspacePublish: true,
			artifactHandoff: true,
			toolCallTranscriptRecord: true,
		},
	},
});
assert.deepEqual(expandAgentTaskCapabilityBundles(['github_publication'], {
	provider_runtime_invocation: { operations: { workspacePublish: { config: { draft: true } } } },
}), {
	tool_presets: ['publication'],
	provider_runtime_invocation: { operations: { workspacePublish: { config: { draft: true } } } },
});
assert.throws(
	() => expandAgentTaskCapabilityBundles(['product_worktree_pr_iteration']),
	/Unknown agent task capability bundle: product_worktree_pr_iteration/,
);

const runnerSpec = agentTaskRunnerSpec({
	backend: 'wp-codebox',
	runtime: 'wp-codebox',
	config: { provider: 'codex' },
	secretEnv: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
	taskTimeoutSeconds: 900,
	expectedArtifacts: ['patch'],
});
assert.equal(runnerSpec.schema, AGENT_TASK_RUNNER_SPEC_SCHEMA);
assert.throws(
	() => agentTaskRunnerSpec({ config: { provider: 'codex' } }),
	/backend is required/,
);
assert.deepEqual(agentTaskRequestFromRunnerSpec({ runnerSpec }), {
	executor: {
		backend: 'wp-codebox',
		runtime: 'wp-codebox',
		secret_env: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
		config: { provider: 'codex' },
	},
	limits: { timeout_ms: 900000, task_timeout_seconds: 900 },
	artifact_declarations: [],
	expected_artifacts: ['patch'],
});
assert.equal(validateAgentTaskRunnerSpec(runnerSpec), runnerSpec);
assert.throws(
	() => validateAgentTaskRunnerSpec({ schema: AGENT_TASK_RUNNER_SPEC_SCHEMA }),
	/runner spec executor is required/
);

console.log('agent task provider contract smoke passed');
