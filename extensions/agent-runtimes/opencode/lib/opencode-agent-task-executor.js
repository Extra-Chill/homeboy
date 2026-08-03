'use strict';

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	agentTaskProviderContractFields,
	agentTaskPolicyToolPermissions,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require('../../../agent-task-contracts');
const {
	cliAgentTaskSpawnEnv,
	createCliAgentTaskExecutor,
	timeoutSecondsFromLimits,
} = require('../../lib/cli-agent-task-executor');
const {
	createOpenCodeProgressAdapter,
} = require('./opencode-progress-events');

const { spawn, spawnSync } = require('node:child_process');
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
const OPENCODE_FATAL_LOG_PATTERNS = [
	{
		pattern: /\bAI_APICallError\b[\s\S]{0,1000}\b(?:weekly|monthly)\s+limit\s+exhausted\b/i,
		code: 'agent_task.opencode_usage_limit',
		summary: 'OpenCode provider usage limit was reached.',
		classification: 'provider_quota',
	},
	{
		pattern: /\bAI_APICallError\b[\s\S]{0,1000}\bquota\s+(?:is\s+)?(?:exhausted|exceeded)\b/i,
		code: 'agent_task.opencode_usage_limit',
		summary: 'OpenCode provider usage limit was reached.',
		classification: 'provider_quota',
	},
	{
		pattern: /\bAI_APICallError\b[\s\S]{0,1000}\b(?:rate|usage)\s+limit\s+(?:has\s+been\s+)?(?:exhausted|exceeded)\b/i,
		code: 'agent_task.opencode_usage_limit',
		summary: 'OpenCode provider usage limit was reached.',
		classification: 'provider_quota',
	},
	{
		pattern: /\bAI_APICallError\b[\s\S]{0,1000}\busage\s+limit\s+reached\b[\s\S]{0,200}\blimit\s+will\s+reset\b/i,
		code: 'agent_task.opencode_usage_limit',
		summary: 'OpenCode provider usage limit was reached.',
		classification: 'provider_quota',
	},
];
const OPENCODE_PERMISSION_DENIED_PATTERN = /user rejected permission|permission policy rejected|policy denied|permission request denied/i;
const OPENCODE_GIT_CAPTURE_OPTIONS = {
	encoding: 'utf8',
	maxBuffer: 16 * 1024 * 1024,
};
const MAX_STRUCTURED_OUTPUT_BYTES = 64 * 1024;
const MAX_STRUCTURED_ANSWER_BYTES = MAX_STRUCTURED_OUTPUT_BYTES + 16 * 1024;

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
	'live_progress_events',
	'nested_orchestrator',
	'run_scoped_scratch',
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
			candidates: ['opencode'],
			version_command: ['--version'],
			install_hint: 'Install OpenCode or set the generic runtime_bin executor config.',
		},
		remediation: 'Install OpenCode or set the generic runtime_bin executor config.',
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

const OPENCODE_NATIVE_WORKSPACE_PERMISSIONS = {
	readonly: ['read', 'glob', 'grep'],
	readwrite: ['edit', 'bash'],
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

const OPENCODE_SESSION_METADATA_ABSENT = {
	status: 'not_discovered',
	reason: 'OpenCode did not expose stable session or transcript metadata through the executor process contract.',
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
	const config = request.executor?.config || {};
	if (config.inherit_env === true || config.inheritEnv === true) {
		throw new Error('OpenCode ambient env inheritance is not supported; declare env_allowlist, runtime_env, and secret_env explicitly.');
	}
	const env = cliAgentTaskSpawnEnv(request, options, {
		allowlist: OPENCODE_PROCESS_ENV_ALLOWLIST,
		secretEnv: OPENCODE_SECRET_ENV,
	});
	const configContent = opencodeConfigContentForRequest(request, env.OPENCODE_CONFIG_CONTENT);
	return configContent ? { ...env, OPENCODE_CONFIG_CONTENT: configContent } : env;
}

function opencodeConfigContentForRequest(request = {}, existingContent = '') {
	const config = request.executor?.config || {};
	const model = config.model || request.executor?.model || request.model;
	const smallModel = config.small_model || config.smallModel;
	const primaryAgent = config.agent || 'build';

	const content = parseOpenCodeConfigContent(existingContent);
	content.$schema = content.$schema || 'https://opencode.ai/config.json';
	content.agent = objectValue(content.agent);
	delete content.agents;

	if (model) {
		content.model = model;
		content.agent[primaryAgent] = { ...objectValue(content.agent[primaryAgent]), model };
	}

	if (smallModel) {
		content.small_model = smallModel;
	}

	// Homeboy owns durable task identity, so OpenCode must not create a competing
	// provider session title from an ambient or run-scoped configuration layer.
	content.agent.title = { ...objectValue(content.agent.title), disable: true };
	const externalDirectoryPatterns = opencodeExternalDirectoryPatterns(request, config);
	const workspaceReadPatterns = opencodeWorkspaceReadPatterns(request, config);
	if (externalDirectoryPatterns.length > 0) {
		content.permission = permissionWithExternalDirectoryAllowances(content.permission, externalDirectoryPatterns);
		content.agent[primaryAgent] = {
			...objectValue(content.agent[primaryAgent]),
			permission: permissionWithExternalDirectoryAllowances(
				objectValue(content.agent[primaryAgent]).permission,
				externalDirectoryPatterns
			),
		};
	}
	const toolPermissions = agentTaskPolicyToolPermissions(request.policy, {
		native: OPENCODE_NATIVE_WORKSPACE_PERMISSIONS,
		workspace: OPENCODE_WORKSPACE_TOOLS,
	});
	// The native inspection tools remain available for review-only tasks even
	// when their cwd is relative. OpenCode otherwise defaults to `ask`.
	{
		content.permission = permissionWithWorkspaceToolAllowances(content.permission, {}, toolPermissions.native);
		content.agent[primaryAgent] = {
			...objectValue(content.agent[primaryAgent]),
			permission: permissionWithWorkspaceToolAllowances(
				objectValue(content.agent[primaryAgent]).permission,
				content.permission,
				toolPermissions.native
			),
		};
	}

	return JSON.stringify(content);
}

function opencodeExternalDirectoryPatterns(request = {}, config = {}) {
	const workspacePatterns = opencodeWorkspaceReadPatterns(request, config);
	if (workspacePatterns.length === 0) {
		return [];
	}

	const concreteWorkspace = concretePath(resolveOpenCodeCwd(request, config));
	const patterns = [...workspacePatterns];
	const attemptRoot = config.runtime_env?.TMPDIR;
	if (isAbsolutePath(attemptRoot)) {
		const concreteAttemptRoot = concretePath(attemptRoot);
		if (isStrictDescendant(concreteWorkspace, concreteAttemptRoot)) {
			// OpenCode discovers project metadata from this Homeboy-provided attempt root.
			patterns.unshift(path.join(concreteAttemptRoot, '*'));
		}
	}

	return [...new Set(patterns)];
}

function opencodeWorkspaceReadPatterns(request = {}, config = {}) {
	const cwd = resolveOpenCodeCwd(request, config);
	return isAbsolutePath(cwd) ? [path.join(concretePath(cwd), '**')] : [];
}

function concretePath(candidate) {
	const resolved = path.resolve(candidate);
	// OpenCode compares tool paths against the process working directory, which
	// resolves symlinks when the child process starts.
	try {
		return fs.realpathSync.native?.(resolved) || fs.realpathSync(resolved);
	} catch {
		// Keep the configured request path when the caller materializes it after
		// constructing the executor environment.
		return resolved;
	}
}

function isAbsolutePath(value) {
	return typeof value === 'string' && value.trim() !== '' && path.isAbsolute(value);
}

function isStrictDescendant(candidate, parent) {
	const relative = path.relative(parent, candidate);
	return relative !== '' && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function permissionWithExternalDirectoryAllowances(permission, patterns) {
	const rules = typeof permission === 'string'
		? { '*': permission }
		: objectValue(permission);
	const externalDirectory = typeof rules.external_directory === 'string'
		? { '*': rules.external_directory }
		: objectValue(rules.external_directory);

	return {
		...rules,
		external_directory: {
			...externalDirectory,
			// OpenCode resolves matching rules in order, so these task-owned paths
			// deliberately supersede inherited catch-all rules.
			...Object.fromEntries(patterns.map((pattern) => [pattern, 'allow'])),
		},
	};
}

function permissionWithWorkspaceToolAllowances(permission, inheritedPermission = {}, allowedTools) {
	const rules = typeof permission === 'string'
		? { '*': permission }
		: objectValue(permission);
	const permissionTools = Object.values(OPENCODE_NATIVE_WORKSPACE_PERMISSIONS).flat();
	return permissionTools.reduce((nextRules, tool) => ({
		...nextRules,
		[tool]: workspaceToolPermissionRules(rules[tool], inheritedPermission[tool], tool, allowedTools),
	}), rules);
}

function workspaceToolPermissionRules(permission, inheritedPermission, tool, allowedTools) {
	if (Array.isArray(allowedTools) && !allowedTools.includes(tool)) {
		return { '*': 'deny' };
	}
	const inheritedRules = typeof inheritedPermission === 'string'
		? { '*': inheritedPermission }
		: objectValue(inheritedPermission);
	const rules = typeof permission === 'string'
		? { '*': permission }
		: objectValue(permission);
	const deniedRules = Object.fromEntries(
		Object.entries({ ...inheritedRules, ...rules }).filter(([pattern, action]) => pattern !== '*' && action === 'deny')
	);
	return {
		// These surfaces correspond to the executor's declared Homeboy workspace
		// capabilities. OpenCode evaluates their paths from the task cwd.
		'*': 'allow',
		...(tool === 'read' ? { '..': 'deny', '../*': 'deny', '..\\*': 'deny' } : {}),
		...deniedRules,
	};
}

function parseOpenCodeConfigContent(content) {
	if (!content || typeof content !== 'string') {
		return {};
	}
	try {
		const parsed = JSON.parse(content);
		return objectValue(parsed);
	} catch {
		return {};
	}
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
	return uniqueDeclaredArtifactRequirements(request).filter((declaration) => !findArtifact(artifacts, declaration));
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
	const structured = structuredOpenCodeOutputs(context);
	const evidence = collectOpenCodeArtifacts(context, structured);
	const emptyPatch = evidence.artifacts?.some((artifact) => artifact.name === 'patch' && artifact.bytes === 0);
	const missingRequiredOutputs = structured.missingRequiredOutputs || [];
	const intentionalNoChange = structured.outputs && missingRequiredOutputs.length === 0 && emptyPatch && context.initialRevision?.revision
		? {
			schema: 'homeboy/intentional-no-change/v1',
			verdict: 'no_change',
			inspected_revision: context.initialRevision.revision,
		}
		: null;
	return withPolicyDeniedOutcome(context, {
		status: missingRequiredOutputs.length > 0 ? 'failed' : 'succeeded',
		...(missingRequiredOutputs.length > 0 ? {
			failure_classification: 'provider',
			failure_code: 'agent_task.opencode_required_outputs_missing',
		} : {}),
		summary: missingRequiredOutputs.length > 0
			? `OpenCode completed without required structured output(s): ${missingRequiredOutputs.join(', ')}.`
			: intentionalNoChange
			? 'OpenCode completed without workspace changes.'
			: 'OpenCode completed successfully.',
		diagnostics: [
			{ classification: 'provider', message: 'OpenCode CLI exited with status 0.' },
			...(structured.diagnostics || []),
		],
		metadata: {
			exit_code: 0,
			opencode_session: sessionMetadata(context),
			opencode_progress: progressMetadata(context),
		},
		...(structured.outputs || intentionalNoChange ? {
			outputs: {
				...(structured.outputs || {}),
				...(intentionalNoChange ? {
					opencode_run_result: {
						status: 'succeeded',
						intentional_no_change: intentionalNoChange,
					},
				} : {}),
			},
		} : {}),
		...evidence,
	});
}

function structuredOpenCodeOutputs(context = {}) {
	const declarations = outputDeclarations(context.request);
	if (declarations.length === 0) {
		return {};
	}
	const textEvents = parseJsonObjectsFromText(context.spawnResult?.stdout)
		.flatMap(openCodeTextParts);
	const finalAnswers = textEvents.filter((event) => event.final_answer);
	const candidates = finalAnswers.length > 0 ? finalAnswers : textEvents.slice(-1);
	for (const event of candidates.reverse()) {
		const envelope = parseStructuredAnswer(event.text);
		const values = declaredOutputValues(envelope, declarations);
		if (!values) {
			continue;
		}
		const outputs = {};
		const oversized = [];
		for (const declaration of declarations) {
			if (!Object.hasOwn(values, declaration.name)) {
				continue;
			}
			if (boundedStructuredOutput(values[declaration.name])) {
				// Core owns declaration-schema validation; retain bounded provider values.
				outputs[declaration.name] = values[declaration.name];
			} else {
				oversized.push(declaration.name);
			}
		}
		const missing = declarations.filter((declaration) => declaration.required && !Object.hasOwn(outputs, declaration.name));
		return {
			...(Object.keys(outputs).length > 0 ? { outputs } : {}),
			missingRequiredOutputs: missing.map((declaration) => declaration.name),
			diagnostics: outputDiagnostics(missing, oversized),
		};
	}
	const missing = declarations.filter((declaration) => declaration.required);
	return {
		missingRequiredOutputs: missing.map((declaration) => declaration.name),
		diagnostics: outputDiagnostics(missing, []),
	};
}

function declaredOutputValues(envelope, declarations) {
	if (!envelope || typeof envelope !== 'object' || Array.isArray(envelope)) {
		return null;
	}
	// OpenCode's current prompt contract is the canonical `outputs` envelope.
	if (envelope.outputs && typeof envelope.outputs === 'object' && !Array.isArray(envelope.outputs)
		&& declarations.some((declaration) => Object.hasOwn(envelope.outputs, declaration.name))) {
		return envelope.outputs;
	}
	// Persisted pre-envelope recipes can only expose their explicitly declared names.
	const legacy = Object.fromEntries(declarations
		.filter((declaration) => Object.hasOwn(envelope, declaration.name))
		.map((declaration) => [declaration.name, envelope[declaration.name]]));
	return Object.keys(legacy).length > 0 ? legacy : null;
}

function boundedStructuredOutput(value) {
	try {
		return Buffer.byteLength(JSON.stringify(value)) <= MAX_STRUCTURED_OUTPUT_BYTES;
	} catch {
		return false;
	}
}

function openCodeTextParts(frame = {}) {
	const parts = Array.isArray(frame.parts) ? frame.parts : [frame.part || frame];
	return parts
		.filter((part) => part && typeof part === 'object' && (part.type === 'text' || frame.type === 'text'))
		.map((part) => ({
			text: stringValue(part.text),
			final_answer: part.metadata?.openai?.phase === 'final_answer'
				|| frame.metadata?.openai?.phase === 'final_answer',
		}))
		.filter((event) => event.text);
}

function parseStructuredAnswer(text = '') {
	const candidates = [String(text).trim()];
	for (const match of String(text).matchAll(/```(?:json)?\s*([\s\S]*?)```/gi)) {
		candidates.unshift(match[1].trim());
	}
	for (const candidate of candidates) {
		if (Buffer.byteLength(candidate) > MAX_STRUCTURED_ANSWER_BYTES) {
			continue;
		}
		try {
			const parsed = JSON.parse(candidate);
			if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
				return parsed;
			}
		} catch {
			// Continue through the bounded final-answer candidates.
		}
	}
	return null;
}

function outputDeclarations(request = {}) {
	const direct = arrayValue(request.output_declarations);
	const directNames = new Set(direct.map((declaration) => declaration?.name).filter(Boolean));
	return [...arrayValue(request.inputs?.required_outputs).filter((declaration) => !directNames.has(declaration?.name)), ...direct]
		.filter((declaration) => declaration && typeof declaration === 'object'
			&& typeof declaration.name === 'string' && declaration.name.trim() !== '')
		.map((declaration) => {
			const { structural_schema: structuralSchema, ...normalized } = declaration;
			return {
				...normalized,
				name: declaration.name.trim(),
				required: declaration.required === true,
				...(declaration.json_schema === undefined && structuralSchema !== undefined
					? { json_schema: structuralSchema }
					: {}),
			};
		});
}

function outputDiagnostics(missing, oversized) {
	return [
		...(missing.length > 0 ? [{
			class: 'opencode.required_outputs_missing',
			classification: 'provider',
			message: `OpenCode completed without required structured output(s): ${missing.map((declaration) => declaration.name).join(', ')}.`,
			data: { missing_outputs: missing.map((declaration) => declaration.name) },
		}] : []),
		...(oversized.length > 0 ? [{
			class: 'opencode.declared_outputs_oversized',
			classification: 'provider',
			message: `OpenCode emitted structured output(s) exceeding the size limit: ${oversized.join(', ')}.`,
			data: { oversized_outputs: oversized },
		}] : []),
	];
}

function opencodeFailureOutcome(context) {
	return withPolicyDeniedOutcome(context, {
		status: 'failed',
		failure_classification: 'execution_failed',
		failure_code: 'agent_task.opencode_failed',
		summary: 'OpenCode execution failed.',
		diagnostics: [{ classification: 'execution_failed', message: `OpenCode CLI exited with status ${context.spawnResult.status}.` }],
		metadata: {
			exit_code: context.spawnResult.status,
			...(context.spawnResult.signal ? { signal: context.spawnResult.signal } : {}),
			opencode_session: sessionMetadata(context),
			opencode_progress: progressMetadata(context),
		},
		...collectOpenCodeArtifacts(context),
	});
}

function withPolicyDeniedOutcome(context = {}, terminal = {}) {
	const denial = detectOpenCodePolicyDenial(context);
	if (!denial) {
		return terminal;
	}
	return {
		...terminal,
		status: 'failed',
		failure_classification: 'policy_denied',
		failure_code: 'agent_task.opencode_policy_denied',
		summary: 'OpenCode stopped after a permission policy denial.',
		diagnostics: [
			...(Array.isArray(terminal.diagnostics) ? terminal.diagnostics : []),
			{
				class: 'opencode.policy_denied',
				classification: 'policy_denied',
				message: deniedToolCallSummary(denial),
				data: { denied_tool_call: denial },
			},
		],
		metadata: {
			...(terminal.metadata || {}),
			denied_tool_call: denial,
		},
	};
}

function deniedToolCallSummary(denial = {}) {
	const details = [
		denial.tool,
		denial.permission && denial.path ? `${denial.permission}: ${denial.path}` : denial.permission,
		denial.command,
	].filter(Boolean).join(': ');
	return details ? `OpenCode permission policy denied tool call ${details}.` : 'OpenCode permission policy denied a tool call.';
}

function detectOpenCodePolicyDenial(context = {}) {
	const spawnResult = context.spawnResult || {};
	const text = redactKnownSecrets([
		spawnResult.stdout,
		spawnResult.stderr,
	].filter(Boolean).join('\n'), context.spawnExtra?.env);
	if (!OPENCODE_PERMISSION_DENIED_PATTERN.test(text)) {
		return null;
	}
	if (spawnResult.status === 0 && openCodeCompletedAfterPolicyDenial(text)) {
		return null;
	}
	return sanitizeDeniedToolCall(extractDeniedToolCall(text));
}

function openCodeCompletedAfterPolicyDenial(text = '') {
	const lines = String(text).split(/\r?\n/).filter((line) => line.trim() !== '');
	const deniedIndex = lines.findLastIndex((line) => OPENCODE_PERMISSION_DENIED_PATTERN.test(line));
	return lines.slice(deniedIndex + 1).some((line) => {
		try {
			const value = JSON.parse(line);
			return openCodeTextParts(value).some((event) => (
				event.text !== '' && !OPENCODE_PERMISSION_DENIED_PATTERN.test(event.text)
			));
		} catch {
			return false;
		}
	});
}

function extractDeniedToolCall(text = '') {
	const externalDirectory = String(text).match(/permission requested:\s*([\w-]+)\s*\(([^)]+)\)/i);
	const requestedPermission = externalDirectory ? {
		permission: externalDirectory[1],
		path: externalDirectory[2],
	} : {};
	const parsed = parseJsonObjectsFromText(text);
	for (const value of parsed) {
		const found = findDeniedToolCall(value);
		if (found) {
			return { ...found, ...requestedPermission };
		}
	}
	return {
		tool: firstRegexCapture(text, /"(?:tool|name)"\s*:\s*"([^"]+)"/),
		command: firstRegexCapture(text, /"command"\s*:\s*"([^"]+)"/),
		timestamp: firstRegexCapture(text, /(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)/),
		...requestedPermission,
	};
}

function parseJsonObjectsFromText(text = '') {
	return String(text).split(/\r?\n/).map((line) => {
		const trimmed = line.trim();
		if (!trimmed.startsWith('{') || !trimmed.endsWith('}')) {
			return null;
		}
		try {
			return JSON.parse(trimmed);
		} catch {
			return null;
		}
	}).filter(Boolean);
}

function findDeniedToolCall(value, inherited = {}) {
	if (!value || typeof value !== 'object') {
		return null;
	}
	const current = {
		tool: stringValue(value.tool || value.name || inherited.tool),
		command: stringValue(value.command || value.input?.command || inherited.command),
		timestamp: stringValue(value.timestamp || value.time?.created || value.created || value.created_at || value.time || inherited.timestamp),
	};
	const message = stringValue(value.error || value.message || value.text || value.output || value.state?.error || value.result?.error);
	if (OPENCODE_PERMISSION_DENIED_PATTERN.test(message)) {
		return current;
	}
	for (const child of Object.values(value)) {
		if (Array.isArray(child)) {
			for (const item of child) {
				const found = findDeniedToolCall(item, current);
				if (found) {
					return found;
				}
			}
		} else if (child && typeof child === 'object') {
			const found = findDeniedToolCall(child, current);
			if (found) {
				return found;
			}
		}
	}
	return null;
}

function sanitizeDeniedToolCall(value = {}) {
	return Object.fromEntries(Object.entries({
		tool: stringValue(value.tool),
		command: stringValue(value.command),
		timestamp: stringValue(value.timestamp),
		permission: stringValue(value.permission),
		path: stringValue(value.path),
	}).filter(([, entry]) => entry));
}

function firstRegexCapture(text, pattern) {
	const match = String(text || '').match(pattern);
	return match ? match[1] : '';
}

function stringValue(value) {
	return typeof value === 'string' ? value : '';
}

function collectOpenCodeArtifacts(context = {}, structured = {}) {
	const request = context.request || {};
	const artifactDir = resolveArtifactDir(context);
	if (!artifactDir) {
		return artifactCaptureFailure('OpenCode artifact directory could not be resolved.');
	}

	const requirements = canonicalArtifactRequirements(request);

	const artifacts = [];
	const evidence_refs = [];
	const captureErrors = [];
	const addArtifact = (requirement, filename, content, fallbackKind) => {
		if (content === undefined || content === null) {
			return;
		}
		const artifactContent = String(content);
		const filePath = path.join(artifactDir, filename);
		try {
			fs.mkdirSync(path.dirname(filePath), { recursive: true });
			fs.writeFileSync(filePath, artifactContent);
		} catch (error) {
			captureErrors.push({ artifact: requirement.name, message: error.message });
			return;
		}
		const artifact = {
			id: requirement.name,
			name: requirement.name,
			kind: requirement.kind || fallbackKind,
			path: filePath,
			bytes: Buffer.byteLength(artifactContent),
		};
		artifacts.push(artifact);
		evidence_refs.push({ kind: artifact.kind, label: requirement.name, uri: fileUri(filePath) });
	};

	const spawnResult = context.spawnResult || {};
	const stdout = redactKnownSecrets(String(spawnResult.stdout || ''), context.spawnExtra?.env);
	const stderr = redactKnownSecrets(String(spawnResult.stderr || ''), context.spawnExtra?.env);
	const transcript = [stdout, stderr].filter(Boolean).join('\n');
	const patchCapture = gitDiff(context.cwd, context.initialRevision);
	if (patchCapture.error) {
		captureErrors.push({ artifact: 'patch', message: patchCapture.error });
	}
	const patch = patchCapture.content;
	const policyDenial = detectOpenCodePolicyDenial(context);
	const missingRequiredOutputs = structured.missingRequiredOutputs || [];
	const intentionalNoChange = structured.outputs && missingRequiredOutputs.length === 0 && patch === '' && context.initialRevision?.revision
		? {
			schema: 'homeboy/intentional-no-change/v1',
			verdict: 'no_change',
			inspected_revision: context.initialRevision.revision,
		}
		: null;
	const resultStatus = policyDenial
		? 'failed'
		: (spawnResult.status === 0 && missingRequiredOutputs.length === 0 ? 'succeeded' : 'failed');
	const resultEnvelope = JSON.stringify({
		schema: 'homeboy/opencode-agent-result/v1',
		task_id: request.task_id,
		status: resultStatus,
		...(policyDenial ? {
			failure_classification: 'policy_denied',
			failure_code: 'agent_task.opencode_policy_denied',
			denied_tool_call: policyDenial,
		} : {}),
		...(!policyDenial && missingRequiredOutputs.length > 0 ? {
			failure_classification: 'provider',
			failure_code: 'agent_task.opencode_required_outputs_missing',
		} : {}),
		...(intentionalNoChange ? { intentional_no_change: intentionalNoChange } : {}),
		exit_code: spawnResult.status,
		command: context.commandSpec?.command,
		args: Array.isArray(context.commandSpec?.args) ? context.commandSpec.args : [],
		artifacts: {
			patch: patch !== '',
			transcript: transcript !== '',
		},
		...(structured.outputs || intentionalNoChange ? {
			outputs: {
				...(structured.outputs || {}),
				...(intentionalNoChange ? {
					opencode_run_result: {
						status: 'succeeded',
						intentional_no_change: intentionalNoChange,
					},
				} : {}),
			},
		} : {}),
		opencode_session: sessionMetadata(context),
		opencode_progress: progressMetadata(context),
	}, null, 2);

	for (const requirement of requirements) {
		if (requirement.name === 'patch') {
			addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode.patch`, patch, 'git-diff');
		} else if (requirement.name === 'transcript') {
			addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode-transcript.txt`, transcript, 'agent-runtime-transcript');
		} else if (requirement.name === 'agent_result') {
			addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode-result.json`, `${resultEnvelope}\n`, 'json');
		} else if (requirement.name === 'progress_events') {
			const progressPath = context.progressEventPath;
			if (progressPath && fs.existsSync(progressPath) && fs.statSync(progressPath).size > 0) {
				const progress = fs.readFileSync(progressPath, 'utf8');
				addArtifact(requirement, `${safeFileSegment(request.task_id)}-opencode-progress.jsonl`, progress, 'agent-task-progress');
			}
		}
	}

	return captureErrors.length === 0
		? { artifacts, evidence_refs }
		: { artifacts, evidence_refs, artifact_capture_errors: captureErrors };
}

function artifactCaptureFailure(message) {
	return { artifacts: [], evidence_refs: [], artifact_capture_errors: [{ artifact: 'artifact_root', message }] };
}

function collectOpenCodeRuntimeLogs(context = {}) {
	const artifactDir = resolveArtifactDir(context);
	if (!artifactDir) {
		return {};
	}

	const artifacts = [];
	const evidence_refs = [];
	for (const stream of ['stdout', 'stderr']) {
		const filePath = context.runtimeLogPaths?.[stream];
		if (!filePath || !fs.existsSync(filePath)) {
			continue;
		}
		const bytes = fs.statSync(filePath).size;
		if (bytes === 0) {
			continue;
		}
		const artifact = {
			id: `opencode-runtime-${stream}`,
			name: `opencode-runtime-${stream}`,
			kind: 'opencode-runtime-log',
			stream,
			path: filePath,
			bytes,
		};
		artifacts.push(artifact);
		evidence_refs.push({ kind: artifact.kind, label: `OpenCode ${stream}`, uri: fileUri(filePath) });
	}
	return { artifacts, evidence_refs };
}

function collectOpenCodeProgressEvents(context = {}) {
	const filePath = context.progressEventPath;
	if (!filePath || !fs.existsSync(filePath) || fs.statSync(filePath).size === 0) {
		return {};
	}
	const bytes = fs.statSync(filePath).size;
	return {
		artifacts: [{ id: 'progress_events', name: 'progress_events', kind: 'agent-task-progress', path: filePath, bytes }],
		evidence_refs: [{ kind: 'agent-task-progress', label: 'OpenCode progress events', uri: fileUri(filePath) }],
	};
}

function fileUri(filePath) {
	return `file://${filePath}`;
}

function mergeEvidence(primary = {}, secondary = {}) {
	return {
		artifacts: [...arrayValue(primary.artifacts), ...arrayValue(secondary.artifacts)],
		evidence_refs: [...arrayValue(primary.evidence_refs), ...arrayValue(secondary.evidence_refs)],
	};
}

function sessionMetadata(context = {}) {
	const explicit = context.spawnResult?.opencode_session || context.spawnResult?.opencodeSession;
	return explicit && typeof explicit === 'object' && !Array.isArray(explicit) ? explicit : OPENCODE_SESSION_METADATA_ABSENT;
}

function progressMetadata(context = {}) {
	return context.spawnResult?.progress || { emitted: 0, coalesced_or_dropped: 0, last_type: '' };
}

function gitRevision(cwd) {
	if (!cwd) {
		return { revision: '', error: 'OpenCode workspace path was not available for patch capture.' };
	}
	const result = spawnSync('git', ['rev-parse', '--verify', 'HEAD'], { cwd, ...OPENCODE_GIT_CAPTURE_OPTIONS });
	if (result.status !== 0 || result.error) {
		return { revision: '', error: result.error?.message || String(result.stderr || 'git rev-parse failed').trim() };
	}
	return { revision: String(result.stdout || '').trim(), error: null };
}

function gitDiff(cwd, initialRevision = {}) {
	if (!cwd) {
		return { content: '', error: 'OpenCode workspace path was not available for patch capture.' };
	}
	if (!initialRevision.revision) {
		return { content: '', error: initialRevision.error || 'OpenCode workspace initial revision was not available for patch capture.' };
	}
	const result = spawnSync('git', ['diff', '--no-ext-diff', '--binary', initialRevision.revision], { cwd, ...OPENCODE_GIT_CAPTURE_OPTIONS });
	if (result.status !== 0 || result.error) {
		return { content: '', error: result.error?.message || String(result.stderr || 'git diff failed').trim() };
	}
	let content = String(result.stdout || '');
	const untracked = spawnSync('git', ['ls-files', '--others', '--exclude-standard', '-z'], { cwd, ...OPENCODE_GIT_CAPTURE_OPTIONS });
	if (untracked.status !== 0 || untracked.error) {
		return { content, error: untracked.error?.message || String(untracked.stderr || 'git ls-files failed').trim() };
	}
	for (const relativePath of String(untracked.stdout || '').split('\0').filter(Boolean)) {
		const untrackedDiff = spawnSync('git', ['diff', '--no-ext-diff', '--binary', '--no-index', '--', '/dev/null', relativePath], { cwd, ...OPENCODE_GIT_CAPTURE_OPTIONS });
		if (![0, 1].includes(untrackedDiff.status) || untrackedDiff.error) {
			return { content, error: untrackedDiff.error?.message || String(untrackedDiff.stderr || `failed to capture untracked file ${relativePath}`).trim() };
		}
		content += String(untrackedDiff.stdout || '');
	}
	return { content, error: null };
}

function canonicalArtifactRequirements(request = {}) {
	const canonical = [
		{ name: 'patch', kind: 'git-diff', required: true },
		{ name: 'transcript', kind: 'agent-runtime-transcript', required: true },
		{ name: 'agent_result', kind: 'json', required: true },
		{ name: 'progress_events', kind: 'agent-task-progress', required: false },
	];
	const declared = uniqueDeclaredArtifactRequirements(request);
	return [...canonical, ...declared].filter((requirement, index, all) => (
		all.findIndex((candidate) => candidate.name === requirement.name) === index
	));
}

function withArtifactCaptureFailure(terminal = {}) {
	const errors = arrayValue(terminal.artifact_capture_errors);
	if (errors.length === 0) {
		return terminal;
	}
	const message = `OpenCode artifact capture failed: ${errors.map((error) => `${error.artifact}: ${error.message}`).join('; ')}`;
	return {
		...terminal,
		status: 'provider_error',
		failure_classification: 'provider',
		failure_code: 'agent_task.opencode_artifact_capture_failed',
		summary: message,
		diagnostics: [...arrayValue(terminal.diagnostics), { class: 'opencode.artifact_capture_failed', classification: 'provider', message, data: { errors } }],
		metadata: { ...(terminal.metadata || {}), artifact_capture_errors: errors },
	};
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
	const configuredCommand = options.command || config.runtime_bin || config.runtimeBin || config.command || 'opencode';
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs();
	if (typeof configuredCommand !== 'string' || configuredCommand.trim() === '') {
		return { error: 'executor.config.command must be a non-empty string when provided.' };
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function opencodeRunArgs(request = {}, config = {}, commandSpec = {}) {
	const model = config.model || request.executor?.model || request.model;
	const format = config.format || 'json';
	return [
		...arrayValue(commandSpec.args),
		'run',
		...(format ? ['--format', format] : []),
		...(model ? ['--model', model] : []),
		...(config.agent ? ['--agent', config.agent] : []),
		...(config.variant ? ['--variant', config.variant] : []),
		...(config.title ? ['--title', config.title] : []),
		`${request.instructions}${requiredOutputInstructions(request)}`,
	];
}

function requiredOutputInstructions(request = {}) {
	const declarations = outputDeclarations(request);
	if (declarations.length === 0) {
		return '';
	}
	return `\n\nReturn one JSON object in your final answer with declared values under \`outputs\`. Include every required declaration and any optional declaration you produced. Output declarations: ${JSON.stringify(declarations)}.`;
}

function resolveOpenCodeCwd(request = {}, config = {}) {
	return config.cwd
		|| config.workspace_root
		|| config.workspaceRoot
		|| request.workspace_path
		|| request.workspace?.path
		|| request.workspace?.root
		|| process.cwd();
}

async function executeOpenCodeAgentTask(request = {}, options = {}) {
	const validationError = validateOpenCodeRequest(request);
	if (validationError) {
		return validationFailure(request, validationError);
	}

	const config = request.executor.config || {};
	const commandSpec = resolveCommandSpec(config, options);
	if (commandSpec.error) {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: commandSpec.classification || 'provider',
			failure_code: commandSpec.code || 'agent_task.invalid_opencode_command',
			summary: commandSpec.summary || 'OpenCode command configuration is invalid.',
			diagnostics: [{ classification: 'provider_setup', message: commandSpec.error }],
		});
	}

	const cwd = resolveOpenCodeCwd(request, config);
	const args = opencodeRunArgs(request, config, commandSpec);
	const spawnExtra = { env: { ...opencodeSpawnEnv(request, options), PWD: cwd } };
	const timeoutSeconds = timeoutSecondsFromLimits(request.limits, config.timeout_seconds);
	const runtimeLogPaths = openCodeRuntimeLogPaths(request, config);
	const progressEventPath = openCodeProgressEventPath(request, config);
	const initialRevision = gitRevision(cwd);
	const spawnResult = await spawnOpenCodeStreaming(commandSpec.command, args, {
		cwd,
		env: spawnExtra.env,
		timeoutMs: timeoutSeconds > 0 ? timeoutSeconds * 1000 : 0,
		runtimeLogPaths,
		diagnosticLogPath: openCodeDiagnosticLogPath(config, spawnExtra.env),
		progress: createOpenCodeProgressAdapter({
			taskId: request.task_id,
			cwd,
			env: spawnExtra.env,
			filePath: progressEventPath,
			maxEvents: config.max_progress_events || config.maxProgressEvents,
			onProgress: options.onProgress || options.on_progress,
		}),
	});
	const context = { request, config, commandSpec, cwd, initialRevision, spawnResult, spawnExtra, runtimeLogPaths, progressEventPath };
	const runtimeLogs = collectOpenCodeRuntimeLogs(context);
	const progressEvidence = collectOpenCodeProgressEvents(context);

	if (spawnResult.error?.code === 'ENOENT') {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.opencode_command_not_found',
			summary: 'OpenCode command was not found.',
			diagnostics: [{ classification: 'provider_setup', message: 'Install opencode or configure executor.config.command.' }],
			metadata: { opencode_progress: progressMetadata(context) },
			...mergeEvidence(runtimeLogs, progressEvidence),
		});
	}

	if (spawnResult.error) {
		const timedOut = spawnResult.error.code === 'ETIMEDOUT';
		const fatalRuntimeError = spawnResult.fatalRuntimeError;
		return outcome(request, {
			status: timedOut ? 'timeout' : 'provider_error',
			failure_classification: fatalRuntimeError?.classification || (timedOut ? 'timeout' : 'provider'),
			failure_code: fatalRuntimeError?.code || (timedOut ? 'agent_task.opencode_timeout' : 'agent_task.opencode_spawn_failed'),
			summary: fatalRuntimeError?.summary || (timedOut ? 'OpenCode execution timed out.' : 'OpenCode process failed to start or complete.'),
			diagnostics: [{
				class: fatalRuntimeError ? 'opencode.provider_quota' : undefined,
				classification: fatalRuntimeError?.classification || (timedOut ? 'timeout' : 'provider_setup'),
				message: spawnResult.error.message,
			}],
			metadata: { opencode_session: sessionMetadata(context), opencode_progress: progressMetadata(context) },
			...mergeEvidence(runtimeLogs, progressEvidence),
		});
	}

	const terminal = withArtifactCaptureFailure(spawnResult.status === 0 ? opencodeSuccessOutcome(context) : opencodeFailureOutcome(context));
	return outcome(request, { ...terminal, ...mergeEvidence(terminal, runtimeLogs) });
}

function validateOpenCodeRequest(request) {
	if (!request || typeof request !== 'object' || Array.isArray(request)) {
		return 'Request must be a JSON object.';
	}
	if (request.schema !== 'homeboy/agent-task-request/v1') {
		return 'Request schema must be homeboy/agent-task-request/v1.';
	}
	if (!request.task_id || typeof request.task_id !== 'string') {
		return 'Request task_id is required.';
	}
	if (request.executor?.backend !== 'opencode') {
		return 'Request executor.backend must be opencode.';
	}
	if (!request.executor.config || typeof request.executor.config !== 'object' || Array.isArray(request.executor.config)) {
		return 'Request executor.config is required.';
	}
	if (!request.instructions || typeof request.instructions !== 'string') {
		return 'Request instructions are required.';
	}
	if (request.executor.config.inherit_env === true || request.executor.config.inheritEnv === true) {
		return 'executor.config.inherit_env is not supported; declare env_allowlist, runtime_env, and secret_env explicitly.';
	}
	return null;
}

function openCodeRuntimeLogPaths(request = {}, config = {}) {
	const artifactDir = resolveArtifactDir({ request, config });
	if (!artifactDir) {
		return {};
	}
	return {
		stdout: path.join(artifactDir, `${safeFileSegment(request.task_id)}-opencode-runtime-stdout.log`),
		stderr: path.join(artifactDir, `${safeFileSegment(request.task_id)}-opencode-runtime-stderr.log`),
	};
}

function openCodeProgressEventPath(request = {}, config = {}) {
	const configured = config.progress_events_path || config.progressEventsPath || request.progress_events_path || request.progressEventsPath || process.env.HOMEBOY_AGENT_TASK_PROGRESS_EVENTS_FILE || '';
	if (configured) {
		return configured;
	}
	const artifactDir = resolveArtifactDir({ request, config });
	return artifactDir ? path.join(artifactDir, `${safeFileSegment(request.task_id)}-opencode-progress.jsonl`) : '';
}

function openCodeDiagnosticLogPath(config = {}, env = process.env) {
	const configured = config.diagnostic_log_path || config.diagnosticLogPath || env.HOMEBOY_OPENCODE_LOG_PATH || '';
	if (configured) {
		return configured;
	}
	const home = env.HOME || process.env.HOME || '';
	return home ? path.join(home, '.local', 'share', 'opencode', 'log', 'opencode.log') : '';
}

function resolveArtifactDir(context = {}) {
	const request = context.request || {};
	const config = context.config || {};
	const configured = config.artifacts_path || config.artifactsPath || request.artifacts_path || process.env.HOMEBOY_AGENT_TASK_ARTIFACTS_DIR || process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACTS_DIR || '';
	if (configured) {
		return configured;
	}
	const workspacePath = config.workspace_root || config.workspaceRoot || request.workspace_path || request.workspace?.path || request.workspace?.root || '';
	return workspacePath ? path.join(workspacePath, '.homeboy', 'opencode') : '';
}

function spawnOpenCodeStreaming(command, args, options = {}) {
	return new Promise((resolve) => {
		const stdoutChunks = [];
		const stderrChunks = [];
		const fatalStreamBuffers = { stdout: '', stderr: '' };
		let settled = false;
		let timedOut = false;
		let fatalRuntimeError = null;
		let child;
		let timer = null;
		let exitFallbackTimer = null;
		let diagnosticLogTimer = null;
		let diagnosticLogOffset = diagnosticLogInitialOffset(options.diagnosticLogPath);

		for (const filePath of Object.values(options.runtimeLogPaths || {})) {
			fs.mkdirSync(path.dirname(filePath), { recursive: true });
			fs.writeFileSync(filePath, '');
		}

		const reportFatalRuntimeError = (detected) => {
			if (fatalRuntimeError) {
				return;
			}
			fatalRuntimeError = detected;
			child.kill('SIGTERM');
		};

		const append = (stream, chunk) => {
			const content = redactKnownSecrets(chunk.toString('utf8'), options.env);
			if (stream === 'stdout') {
				stdoutChunks.push(content);
			} else {
				stderrChunks.push(content);
			}
			const filePath = options.runtimeLogPaths?.[stream];
			if (filePath) {
				fs.appendFileSync(filePath, content);
			}
			options.progress?.consume(stream, content);
			fatalStreamBuffers[stream] = `${fatalStreamBuffers[stream]}${content}`.slice(-1200);
			const matched = findOpenCodeFatalRuntimeError(fatalStreamBuffers[stream]);
			if (matched) {
				reportFatalRuntimeError(matched);
			}
		};

		const finish = (result) => {
			if (settled) {
				return;
			}
			settled = true;
			if (exitFallbackTimer) {
				clearTimeout(exitFallbackTimer);
			}
			if (diagnosticLogTimer) {
				clearInterval(diagnosticLogTimer);
			}
			if (timer) {
				clearTimeout(timer);
			}
			resolve({
				stdout: stdoutChunks.join(''),
				stderr: stderrChunks.join(''),
				progress: options.progress?.finish() || { emitted: 0, coalesced_or_dropped: 0, last_type: '' },
				...(fatalRuntimeError ? { fatalRuntimeError } : {}),
				...result,
			});
		};

		try {
			child = spawn(command, args, { cwd: options.cwd, env: options.env, stdio: ['ignore', 'pipe', 'pipe'] });
		} catch (error) {
			finish({ error });
			return;
		}

		timer = options.timeoutMs > 0 ? setTimeout(() => {
			timedOut = true;
			child.kill('SIGTERM');
		}, options.timeoutMs) : null;
		diagnosticLogTimer = startOpenCodeDiagnosticLogMonitor({
			logPath: options.diagnosticLogPath,
			initialOffset: diagnosticLogOffset,
			env: options.env,
			onFatal: (detected) => {
				reportFatalRuntimeError(detected);
			},
		});

		child.stdout.on('data', (chunk) => append('stdout', chunk));
		child.stderr.on('data', (chunk) => append('stderr', chunk));
		child.on('error', (error) => finish({ error }));
		child.on('exit', (status, signal) => {
			// Some runtimes leave descendants holding inherited stdio open after the
			// command exits. Fall back to process exit so Homeboy does not wait forever
			// for a `close` event that cannot arrive until those descendants die.
			exitFallbackTimer = setTimeout(() => {
				child.stdout.destroy();
				child.stderr.destroy();
				finish({
					status,
					signal,
					...(fatalRuntimeError ? { error: Object.assign(new Error(fatalRuntimeError.message), { code: 'EOPENCODEFATAL' }) } : {}),
					...(timedOut ? { error: Object.assign(new Error('OpenCode execution timed out.'), { code: 'ETIMEDOUT' }) } : {}),
				});
			}, 250);
		});
		child.on('close', (status, signal) => {
			finish({
				status,
				signal,
				...(fatalRuntimeError ? { error: Object.assign(new Error(fatalRuntimeError.message), { code: 'EOPENCODEFATAL' }) } : {}),
				...(timedOut ? { error: Object.assign(new Error('OpenCode execution timed out.'), { code: 'ETIMEDOUT' }) } : {}),
			});
		});
	});
}

function diagnosticLogInitialOffset(logPath) {
	try {
		if (!logPath || !fs.existsSync(logPath)) {
			return 0;
		}
		return fs.statSync(logPath).size;
	} catch {
		return 0;
	}
}

function startOpenCodeDiagnosticLogMonitor({ logPath, initialOffset = 0, env = process.env, onFatal }) {
	if (!logPath || typeof onFatal !== 'function') {
		return null;
	}
	let offset = initialOffset;
	let reported = false;
	return setInterval(() => {
		let stat;
		try {
			if (reported || !fs.existsSync(logPath)) {
				return;
			}
			stat = fs.statSync(logPath);
		} catch {
			return;
		}
		if (stat.size < offset) {
			offset = 0;
		}
		if (stat.size === offset) {
			return;
		}

		let fd;
		try {
			fd = fs.openSync(logPath, 'r');
			const length = stat.size - offset;
			const buffer = Buffer.alloc(length);
			fs.readSync(fd, buffer, 0, length, offset);
			offset = stat.size;
			const content = redactKnownSecrets(buffer.toString('utf8'), env);
			const matched = findOpenCodeFatalRuntimeError(content);
			if (matched) {
				reported = true;
				onFatal(matched);
			}
		} finally {
			if (fd !== undefined) {
				fs.closeSync(fd);
			}
		}
	}, 1000);
}

function findOpenCodeFatalRuntimeError(content = '') {
	const matched = OPENCODE_FATAL_LOG_PATTERNS.find(({ pattern }) => pattern.test(content));
	if (!matched) {
		return null;
	}
	return {
		code: matched.code,
		summary: matched.summary,
		classification: matched.classification,
		message: 'OpenCode reported a terminal provider quota limit. Wait for the provider limit to reset or select an available provider/model, then retry.',
	};
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

const { outcome, validationFailure } = createCliAgentTaskExecutor({
	backend: 'opencode',
	providerId: OPENCODE_PROVIDER_ID,
	providerLabel: 'OpenCode agent',
	defaultSummary: 'OpenCode agent task executor failed before producing a detailed outcome.',
	validateRuntime: false,
	finalizeOutcome: withDeclaredArtifactDiagnostics,
	resolveCommandSpec,
	successOutcome: opencodeSuccessOutcome,
	failureOutcome: opencodeFailureOutcome,
	buildArgs: opencodeRunArgs,
	buildSpawn: (request, config, options) => ({
		env: { ...opencodeSpawnEnv(request, options), PWD: resolveOpenCodeCwd(request, config) },
	}),
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
