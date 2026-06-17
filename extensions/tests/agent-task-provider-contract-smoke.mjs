#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
	AGENT_TASK_ARTIFACT_FIELDS,
	AGENT_TASK_ARTIFACT_SCHEMA,
	AGENT_TASK_EVIDENCE_REF_FIELDS,
	AGENT_TASK_EVIDENCE_REF_SCHEMA,
	AGENT_TASK_REDACTED_METADATA_KEYS,
	AGENT_TASK_SECRET_SELECTOR_PATHS,
	agentTaskArtifactFromRef,
	agentTaskEvidenceRefFromRef,
	agentTaskProviderContractFields,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require(path.join(rootDir, 'agent-runtimes', 'lib', 'agent-task-provider-contract.js'));
const {
	AGENT_TASK_RUNNER_SPEC_SCHEMA,
	agentTaskRequestFromRunnerSpec,
	agentTaskRunnerSpec,
	validateAgentTaskRunnerSpec,
} = require(path.join(rootDir, 'agent-runtimes', 'lib', 'agent-task-runner-contract.js'));

assert.equal(AGENT_TASK_ARTIFACT_SCHEMA, 'homeboy/agent-task-artifact/v1');
assert.equal(AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA, 'homeboy/agent-task-artifact-declaration/v1');
assert.equal(AGENT_TASK_EVIDENCE_REF_SCHEMA, 'homeboy/agent-task-evidence-ref/v1');
assert.deepEqual(AGENT_TASK_ARTIFACT_FIELDS, ['schema', 'id', 'kind', 'name', 'path', 'url', 'mime', 'size_bytes', 'sha256', 'metadata']);
assert.deepEqual(AGENT_TASK_EVIDENCE_REF_FIELDS, ['kind', 'uri', 'label']);

const fields = agentTaskProviderContractFields();
assert.equal(fields.request_schema, 'homeboy/agent-task-request/v1');
assert.equal(fields.outcome_schema, 'homeboy/agent-task-outcome/v1');
assert.deepEqual(fields.redacted_metadata_keys, AGENT_TASK_REDACTED_METADATA_KEYS);

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

const runnerSpec = agentTaskRunnerSpec({
	backend: 'codebox',
	config: { provider: 'codex' },
	secretEnv: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
	taskTimeoutSeconds: 900,
	expectedArtifacts: ['patch'],
});
assert.equal(runnerSpec.schema, AGENT_TASK_RUNNER_SPEC_SCHEMA);
assert.deepEqual(agentTaskRequestFromRunnerSpec({ runnerSpec }), {
	executor: {
		backend: 'codebox',
		secret_env: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
		config: { provider: 'codex' },
	},
	limits: { task_timeout_seconds: 900 },
	expected_artifacts: ['patch'],
});
assert.equal(validateAgentTaskRunnerSpec(runnerSpec), runnerSpec);
assert.throws(
	() => validateAgentTaskRunnerSpec({ schema: AGENT_TASK_RUNNER_SPEC_SCHEMA }),
	/runner spec executor is required/
);

console.log('agent task provider contract smoke passed');
