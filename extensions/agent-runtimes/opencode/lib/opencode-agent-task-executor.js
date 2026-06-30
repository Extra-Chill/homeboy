'use strict';

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	agentTaskProviderContractFields,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require('../../../agent-task-contracts/agent-task-provider-contract');
const {
	createCliAgentTaskExecutor,
} = require('../../lib/cli-agent-task-executor');

const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const OPENCODE_PROVIDER_ID = 'opencode.agent-task-executor';
const OPENCODE_PROVIDER_LABEL = 'OpenCode agent task executor';
const OPENCODE_SECRET_ENV = [
	'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
	'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
	'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];

const OPENCODE_CAPABILITIES = [
	'cli_runtime',
	'repo_workspace',
	'workspace_tools',
	'patch_artifacts',
	'report_artifacts',
	'structured_outcome',
	'provider_owned_auth',
	'provider_owned_session',
	'provider_owned_cancellation',
	'nested_orchestrator',
];

const OPENCODE_COMMAND = 'node {{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs';
const OPENCODE_PROCESS_ENV_ALLOWLIST = [
	'CI',
	'HOME',
	'LANG',
	'LC_ALL',
	'LOGNAME',
	'NODE_OPTIONS',
	'PATH',
	'PWD',
	'SHELL',
	'TMPDIR',
	'USER',
	'HOMEBOY_OPENCODE_COMMAND',
	'HOMEBOY_OPENCODE_COMMAND_ARGS',
];

const OPENCODE_INVOCATION = {
	schema: 'homeboy/command-invocation/v1',
	argv: [
		'node',
		'{{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs',
	],
	display: OPENCODE_COMMAND,
};

const OPENCODE_RUNNER_READINESS = [
	{
		id: 'opencode.executable',
		label: 'OpenCode executable',
		executable: {
			env: ['HOMEBOY_OPENCODE_COMMAND'],
			candidates: ['opencode'],
			version_command: ['--version'],
			install_hint: 'Install OpenCode or set the generic runtime_bin executor config; HOMEBOY_OPENCODE_COMMAND remains a legacy compatibility env alias.',
		},
		remediation: 'Install OpenCode or set the generic runtime_bin executor config; HOMEBOY_OPENCODE_COMMAND remains a legacy compatibility env alias.',
	},
];

const OPENCODE_WORKSPACE_TOOLS = {
	readonly: [
		'workspace_ls',
		'workspace_read',
		'workspace_git_status',
	],
	readwrite: [
		'workspace_run',
		'workspace_write',
		'workspace_edit',
		'workspace_apply_patch',
		'workspace_delete',
		'workspace_git_add',
	],
};

const OPENCODE_WORKSPACE_MATERIALIZATION = {
	cwd: 'git_checkout',
	requires_git: true,
	write_scope: 'workspace',
	artifact_paths: ['.homeboy/opencode'],
};

const OPENCODE_ROLE_ALIASES = {
	artifact_kinds: {
		patch: ['opencode-patch', 'git-diff', 'patch'],
		transcript: ['opencode-transcript', 'agent-runtime-transcript'],
		runtime_log: ['opencode-runtime-log'],
		report: ['opencode-report', 'agent-runtime-report'],
	},
	outputs: {
		provider_run_result: ['opencode_run_result'],
	},
	metadata: {
		provider_run_result: ['opencode_run_result'],
	},
};

const OPENCODE_PROVIDER_DEFAULTS = {
	codex: {
		model: 'gpt-5.5',
		secret_env: [...OPENCODE_SECRET_ENV],
		secret_env_sources: {
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.access_token',
			},
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.refresh_token',
			},
			AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: {
				source: 'json-file-jwt-expiration',
				path: '~/.codex/auth.json',
				field: 'tokens.access_token',
				fallback_fields: ['tokens.expires_at', 'tokens.expiresAt'],
			},
			AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.account_id',
			},
			AI_PROVIDER_OPENAI_CODEX_FEDRAMP: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.fedramp',
				value: 'false',
			},
		},
	},
};

const OPENCODE_PROVIDER_PREFLIGHT = {
	codex: {
		label: 'Codex',
		diagnostic_class: 'opencode.preflight.codex_auth',
		required_secret_env: [
			'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
			'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
			'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
			'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
		],
		optional_secret_env: ['AI_PROVIDER_OPENAI_CODEX_FEDRAMP'],
		refresh_hook: 'codex-oauth-refresh',
		validation_hooks: ['codex-token-expiration'],
		guidance: 'Refresh Codex OAuth credentials before launching OpenCode, for example by signing in with Codex locally so ~/.codex/auth.json contains current tokens, then pass the updated AI_PROVIDER_OPENAI_CODEX_* secret environment values to the OpenCode executor.',
	},
};

function providerContract(options = {}) {
	const contractFields = agentTaskProviderContractFields();
	contractFields.redacted_metadata_keys = extendRedactedMetadataKeys('codex_auth', 'opencode_auth');

	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || OPENCODE_PROVIDER_ID,
		label: options.label || OPENCODE_PROVIDER_LABEL,
		backend: 'opencode',
		runtime_id: 'opencode',
		command: options.command || OPENCODE_COMMAND,
		invocation: options.invocation || OPENCODE_INVOCATION,
		...contractFields,
		secret_env_requirements: [providerSecretEnvRequirement('codex', OPENCODE_SECRET_ENV)],
		capabilities: OPENCODE_CAPABILITIES,
		workspace_materialization: OPENCODE_WORKSPACE_MATERIALIZATION,
		runner_readiness: OPENCODE_RUNNER_READINESS,
		workspace_tools: OPENCODE_WORKSPACE_TOOLS,
		provider_defaults: OPENCODE_PROVIDER_DEFAULTS,
		provider_preflight: OPENCODE_PROVIDER_PREFLIGHT,
		lifecycle: {
			completion: 'synchronous_process',
			cancellation: 'provider_signal',
			max_concurrency_default: 1,
		},
		artifact_contract: {
			patch: ['git-diff', 'patch'],
			report: ['json', 'markdown'],
		},
		role_aliases: OPENCODE_ROLE_ALIASES,
		status: 'active',
		integration_contract: 'homeboy-opencode-agent-task/v1',
	};
}

function opencodeSpawnEnv(request = {}, options = {}) {
	const ambient = options.env && typeof options.env === 'object' && !Array.isArray(options.env) ? options.env : process.env;
	const config = request.executor?.config || {};
	if (config.inherit_env === true || config.inheritEnv === true) {
		return { ...ambient, ...objectValue(config.runtime_env), ...objectValue(options.envOverrides || options.env_overrides) };
	}
	const names = new Set([
		...OPENCODE_PROCESS_ENV_ALLOWLIST,
		...arrayValue(config.env_allowlist || config.envAllowlist),
		...arrayValue(config.runtime_env_allowlist || config.runtimeEnvAllowlist),
		...arrayValue(config.secret_env),
		...arrayValue(request.executor?.secret_env),
	]);
	const env = {};
	for (const name of names) {
		if (typeof name === 'string' && name && ambient[name] !== undefined) {
			env[name] = ambient[name];
		}
	}
	return { ...env, ...objectValue(config.runtime_env), ...objectValue(options.envOverrides || options.env_overrides) };
}

function withDeclaredArtifactDiagnostics(request = {}, normalized = {}) {
	const missing = missingDeclaredArtifacts(request, normalized.artifacts || []);
	if (missing.length === 0) {
		return normalized;
	}
	const diagnostic = {
		class: 'opencode.missing_declared_artifacts',
		message: `OpenCode completed without producing declared artifact(s): ${missing.map((artifact) => artifact.name).join(', ')}. Check executor artifact paths or runtime artifact collection.`,
		data: { missing_artifacts: missing },
	};
	return {
		...normalized,
		status: normalized.status === 'succeeded' ? 'failed' : normalized.status,
		summary: normalized.status === 'succeeded' ? diagnostic.message : normalized.summary,
		failure_classification: normalized.status === 'succeeded' ? 'execution_failed' : normalized.failure_classification,
		failure_code: normalized.status === 'succeeded' ? 'agent_task.opencode_missing_declared_artifacts' : normalized.failure_code,
		diagnostics: [...(Array.isArray(normalized.diagnostics) ? normalized.diagnostics : []), diagnostic],
		metadata: {
			...(normalized.metadata || {}),
			missing_declared_artifacts: missing,
		},
	};
}

function missingDeclaredArtifacts(request = {}, artifacts = []) {
	return declaredArtifactRequirements(request).filter((declaration) => !findArtifact(artifacts, declaration));
}

function declaredArtifactRequirements(request = {}) {
	const expected = arrayValue(request.expected_artifacts).map((name) => ({ name: String(name), required: true }));
	const declared = arrayValue(request.artifact_declarations || request.executor?.artifact_declarations)
		.filter((artifact) => artifact && typeof artifact === 'object' && artifact.required === true)
		.map((artifact) => ({ name: artifact.name || artifact.id || artifact.output_key, kind: artifact.kind, required: true }))
		.filter((artifact) => artifact.name);
	return [...expected, ...declared];
}

function findArtifact(artifacts, declaration) {
	return arrayValue(artifacts).find((artifact) => {
		if (!artifact || typeof artifact !== 'object') {
			return false;
		}
		const names = [artifact.name, artifact.id, artifact.output_key, artifact.role].filter(Boolean);
		return names.includes(declaration.name) && (!declaration.kind || artifact.kind === declaration.kind || artifact.type === declaration.kind);
	});
}

function opencodeSuccessOutcome(context) {
	return {
		status: 'succeeded',
		summary: 'OpenCode completed successfully.',
		diagnostics: [{ classification: 'provider', message: 'OpenCode CLI exited with status 0.' }],
		metadata: { exit_code: 0 },
		...collectOpenCodeArtifacts(context),
	};
}

function collectOpenCodeArtifacts(context = {}) {
	const request = context.request || {};
	const config = context.config || {};
	const artifactDir = config.artifacts_path || config.artifactsPath || request.artifacts_path || process.env.HOMEBOY_AGENT_TASK_ARTIFACTS_DIR || process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACTS_DIR || '';
	if (!artifactDir) {
		return {};
	}

	const requirements = uniqueDeclaredArtifactRequirements(request);
	if (requirements.length === 0) {
		return {};
	}

	const artifacts = [];
	const evidence_refs = [];
	const addArtifact = (requirement, filename, content, fallbackKind) => {
		if (content === undefined || content === null || content === '') {
			return;
		}
		const filePath = path.join(artifactDir, filename);
		fs.mkdirSync(path.dirname(filePath), { recursive: true });
		fs.writeFileSync(filePath, content);
		const artifact = {
			id: requirement.name,
			name: requirement.name,
			kind: requirement.kind || fallbackKind,
			path: filePath,
			bytes: Buffer.byteLength(content),
		};
		artifacts.push(artifact);
		evidence_refs.push({ kind: artifact.kind, label: requirement.name, path: filePath });
	};

	const spawnResult = context.spawnResult || {};
	const stdout = redactKnownSecrets(String(spawnResult.stdout || ''), context.spawnExtra?.env);
	const stderr = redactKnownSecrets(String(spawnResult.stderr || ''), context.spawnExtra?.env);
	const transcript = [stdout, stderr].filter(Boolean).join('\n');
	const patch = gitDiff(context.cwd);
	const resultEnvelope = JSON.stringify({
		schema: 'homeboy/opencode-agent-result/v1',
		task_id: request.task_id,
		status: 'succeeded',
		exit_code: spawnResult.status,
		command: context.commandSpec?.command,
		args: Array.isArray(context.commandSpec?.args) ? context.commandSpec.args : [],
		artifacts: {
			patch: patch !== '',
			transcript: transcript !== '',
		},
	}, null, 2);

	for (const requirement of requirements) {
		if (requirement.name === 'patch') {
			addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode.patch`, patch, 'git-diff');
		} else if (requirement.name === 'transcript') {
			addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode-transcript.txt`, transcript, 'agent-runtime-transcript');
		} else if (requirement.name === 'agent_result') {
			addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode-result.json`, `${resultEnvelope}\n`, 'json');
		}
	}

	return { artifacts, evidence_refs };
}

function gitDiff(cwd) {
	if (!cwd) {
		return '';
	}
	const result = spawnSync('git', ['diff', '--no-ext-diff', '--binary'], { cwd, encoding: 'utf8' });
	if (result.status !== 0 || result.error) {
		return '';
	}
	return String(result.stdout || '');
}

function uniqueDeclaredArtifactRequirements(request = {}) {
	const seen = new Set();
	return declaredArtifactRequirements(request).filter((requirement) => {
		const key = `${requirement.name}:${requirement.kind || ''}`;
		if (seen.has(key)) {
			return false;
		}
		seen.add(key);
		return true;
	});
}

function redactKnownSecrets(content, env = process.env) {
	let redacted = content;
	for (const name of OPENCODE_SECRET_ENV) {
		const value = env?.[name] || process.env[name];
		if (value) {
			redacted = redacted.split(value).join('[redacted]');
		}
	}
	return redacted;
}

function safeFileSegment(value) {
	return String(value || 'agent-task').replace(/[^a-z0-9._-]+/gi, '-').replace(/^-+|-+$/g, '') || 'agent-task';
}

function arrayValue(value) {
	return Array.isArray(value) ? value : [];
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function resolveCommandSpec(config = {}, options = {}) {
	const configuredCommand = options.command || config.runtime_bin || config.runtimeBin || config.command || process.env.HOMEBOY_OPENCODE_COMMAND || 'opencode';
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs();
	if (typeof configuredCommand !== 'string' || configuredCommand.trim() === '') {
		return { error: 'executor.config.command must be a non-empty string when provided.' };
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function parseEnvCommandArgs() {
	if (!process.env.HOMEBOY_OPENCODE_COMMAND_ARGS) {
		return [];
	}
	try {
		const value = JSON.parse(process.env.HOMEBOY_OPENCODE_COMMAND_ARGS);
		return Array.isArray(value) ? value : [];
	} catch {
		return [];
	}
}

const { execute: executeOpenCodeAgentTask, outcome, validationFailure } = createCliAgentTaskExecutor({
	backend: 'opencode',
	providerId: OPENCODE_PROVIDER_ID,
	providerLabel: 'OpenCode agent',
	defaultSummary: 'OpenCode agent task executor failed before producing a detailed outcome.',
	validateRuntime: false,
	finalizeOutcome: withDeclaredArtifactDiagnostics,
	resolveCommandSpec,
	successOutcome: opencodeSuccessOutcome,
	buildArgs: (request, config, commandSpec) => [
		...commandSpec.args,
		'run',
		...(config.model ? ['--model', config.model] : []),
		...(config.agent ? ['--agent', config.agent] : []),
		...(config.variant ? ['--variant', config.variant] : []),
		...(config.title ? ['--title', config.title] : []),
		request.instructions,
	],
	buildSpawn: (request, config, options) => ({ env: opencodeSpawnEnv(request, options) }),
	messages: {
		invalidRequest: { code: 'agent_task.invalid_opencode_request', summary: 'OpenCode request validation failed.' },
		invalidCommand: { code: 'agent_task.invalid_opencode_command', summary: 'OpenCode command configuration is invalid.' },
		notFound: { code: 'agent_task.opencode_command_not_found', summary: 'OpenCode command was not found.', hint: 'Install opencode or configure executor.config.command.' },
		timeout: { code: 'agent_task.opencode_timeout', summary: 'OpenCode execution timed out.' },
		spawnFailed: { code: 'agent_task.opencode_spawn_failed', summary: 'OpenCode process failed to start or complete.' },
		success: { summary: 'OpenCode completed successfully.', diag: 'OpenCode CLI exited with status 0.' },
		failed: { code: 'agent_task.opencode_failed', summary: 'OpenCode execution failed.', diag: (status) => `OpenCode CLI exited with status ${status}.` },
	},
});

module.exports = {
	OPENCODE_PROVIDER_ID,
	OPENCODE_PROVIDER_LABEL,
	OPENCODE_SECRET_ENV,
	OPENCODE_COMMAND,
	OPENCODE_INVOCATION,
	OPENCODE_PROVIDER_DEFAULTS,
	OPENCODE_PROVIDER_PREFLIGHT,
	OPENCODE_ROLE_ALIASES,
	OPENCODE_RUNNER_READINESS,
	OPENCODE_WORKSPACE_TOOLS,
	OPENCODE_WORKSPACE_MATERIALIZATION,
	executeOpenCodeAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
