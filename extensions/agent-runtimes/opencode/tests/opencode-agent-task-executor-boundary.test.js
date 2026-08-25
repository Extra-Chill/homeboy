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
const { agentTaskPolicyToolPermissions } = require('../../../agent-task-contracts');

/**
 * Internal dependencies
 */
const {
	OPENCODE_INVOCATION,
	OPENCODE_PROVIDER_DEFAULTS,
	OPENCODE_PROVIDER_PREFLIGHT,
	OPENCODE_ROLE_ALIASES,
	OPENCODE_RUNNER_READINESS,
	OPENCODE_SECRET_ENV,
	OPENCODE_WORKSPACE_MATERIALIZATION,
	OPENCODE_WORKSPACE_TOOLS,
	executeOpenCodeAgentTask,
	providerContract,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');
const fixtureRuntimeTool = {
	schema: 'homeboy/resolved-agent-task-runtime-tool/v1',
	id: 'fixture.mcp', transport: 'stdio', argv: [process.execPath, '--fixture-mcp', '--isolated'],
	executable: process.execPath, env: { FIXTURE_MODE: 'isolated' }, secret_env_names: ['FIXTURE_MCP_TOKEN'], readiness: { status: 'ready', evidence: { kind: 'version_command', success: true } }, lifecycle: 'runtime_owned',
};
const secondFixtureRuntimeTool = {
	schema: 'homeboy/resolved-agent-task-runtime-tool/v1',
	id: 'fixture.second', transport: 'stdio', argv: [process.execPath, '--second-fixture-mcp'],
	executable: process.execPath, env: {}, secret_env_names: [], readiness: { status: 'ready', evidence: { kind: 'declared_probe', success: true } }, lifecycle: 'runtime_owned',
};

function secretEnvRequirementForProvider(contract, provider) {
	return contract.secret_env_requirements.find((requirement) => (
		requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
	));
}

function opencodeWildcardMatch(input, pattern) {
	let escaped = pattern
		.replaceAll('\\', '/')
		.replace(/[.+^${}()|[\]\\]/g, '\\$&')
		.replace(/\*/g, '.*')
		.replace(/\?/g, '.');
	if (escaped.endsWith(' .*')) escaped = `${escaped.slice(0, -3)}( .*)?`;
	return new RegExp(`^${escaped}$`, process.platform === 'win32' ? 'si' : 's').test(input.replaceAll('\\', '/'));
}

function externalDirectoryAction(config, agent, requestedPattern) {
	const permissions = [config.permission, config.agent?.[agent]?.permission];
	const rules = permissions.flatMap((permission) => Object.entries(permission?.external_directory || {}));
	return rules.findLast(([pattern]) => opencodeWildcardMatch(requestedPattern, pattern))?.[1] || 'ask';
}

function readAction(config, agent, worktree, filepath) {
	// OpenCode evaluates ReadTool rules after resolving the target against its
	// workspace, so absolute workspace rules cannot match nested source files.
	const requestedPath = path.relative(worktree, filepath);
	const permissions = [config.permission, config.agent?.[agent]?.permission];
	const rules = permissions.flatMap((permission) => Object.entries(permission?.read || {}));
	return rules.findLast(([pattern]) => opencodeWildcardMatch(requestedPath, pattern))?.[1] || 'allow';
}

function nativeToolAction(config, agent, tool, requestedValue) {
	const permissions = [config.permission, config.agent?.[agent]?.permission];
	const rules = permissions.flatMap((permission) => Object.entries(permission?.[tool] || {}));
	return rules.findLast(([pattern]) => opencodeWildcardMatch(requestedValue, pattern))?.[1] || 'allow';
}

function concretePath(candidate) {
	try {
		return fs.realpathSync(candidate);
	} catch {
		return path.resolve(candidate);
	}
}

function installedOpenCodeBinary() {
	const result = spawnSync('opencode', ['--version'], { encoding: 'utf8' });
	if (result.error?.code === 'ENOENT') {
		console.log('Skipping OpenCode file resolver integration: opencode binary is unavailable.');
		return null;
	}
	assert.equal(result.status, 0, result.stderr);
	return result.spawnfile || 'opencode';
}

(async () => {
const provider = providerContract();
const policyToolSets = {
	native: { readonly: ['read', 'glob', 'grep'], readwrite: ['edit', 'bash'] },
	workspace: OPENCODE_WORKSPACE_TOOLS,
};
assert.deepEqual(agentTaskPolicyToolPermissions({ write: 'none' }, policyToolSets).native, ['read', 'glob', 'grep']);
assert.deepEqual(agentTaskPolicyToolPermissions({ write: 'none' }, policyToolSets).workspace, OPENCODE_WORKSPACE_TOOLS.readonly);
assert.deepEqual(agentTaskPolicyToolPermissions({ write: 'patch' }, policyToolSets).native, ['read', 'glob', 'grep', 'edit', 'bash']);
assert.deepEqual(agentTaskPolicyToolPermissions({ write: 'patch' }, policyToolSets).workspace, [
	...OPENCODE_WORKSPACE_TOOLS.readonly,
	...OPENCODE_WORKSPACE_TOOLS.readwrite,
]);
assert.equal(provider.id, 'opencode.agent-task-executor');
assert.equal(provider.backend, 'opencode');
assert.equal(provider.runtime_id, 'opencode');
assert.equal(provider.status, 'active');
assert.equal(provider.integration_contract, 'homeboy-opencode-agent-task/v1');
assert.deepEqual(provider.invocation, OPENCODE_INVOCATION);
assert.equal(Object.hasOwn(provider.lifecycle, 'max_concurrency_default'), false);
assert.equal(provider.lifecycle.cancellation, 'provider_signal');
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, OPENCODE_SECRET_ENV);
assert.deepEqual(provider.provider_defaults.codex.secret_env, OPENCODE_SECRET_ENV);
assert.equal(Object.hasOwn(provider.provider_defaults.codex, 'model'), false);
assert.deepEqual(provider.provider_defaults.codex.secret_env_sources, OPENCODE_PROVIDER_DEFAULTS.codex.secret_env_sources);
assert.deepEqual(provider.provider_preflight, OPENCODE_PROVIDER_PREFLIGHT);
assert.deepEqual(provider.runner_readiness, OPENCODE_RUNNER_READINESS);
assert.deepEqual(provider.workspace_materialization, OPENCODE_WORKSPACE_MATERIALIZATION);
assert.deepEqual(provider.workspace_tools, OPENCODE_WORKSPACE_TOOLS);
assert.deepEqual(provider.role_aliases, OPENCODE_ROLE_ALIASES);
assert.equal(provider.redacted_metadata_keys.includes('opencode_auth'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('run_scoped_scratch'), true);
assert.equal(provider.capabilities.includes('runtime_tool_attachment'), true);
assert.equal(provider.capabilities.includes('live_progress_events'), true);
assert.equal(provider.capabilities.includes('workspace_permission_root/v1'), true);
assert.equal(provider.capabilities.includes('workspace_permission_preflight/v1'), true);
assert.equal(provider.capabilities.includes('browser_runtime'), false);

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'opencode.json'), 'utf8'));
assert.equal(manifest.id, 'opencode');
assert.equal(manifest.name, 'OpenCode');
assert.deepEqual(manifest.requires, { homeboy: '>=0.345.0' });
assert.deepEqual(manifest.compatibility, {
	immediate_failure_patterns: {
		owner: 'Extra-Chill/homeboy#12293',
		requirement: 'Homeboy core support for agent-task executor immediate_failure_patterns.',
	},
});
assert.equal(manifest.agent_task_executors.length, 1);
assert.equal(manifest.agent_task_executors[0].capabilities.includes('nested_orchestrator'), true);
assert.equal(Object.hasOwn(manifest.agent_task_executors[0].provider_defaults.codex, 'model'), false);
const [unexpectedServerError] = manifest.agent_task_executors[0].immediate_failure_patterns;
assert.deepEqual(unexpectedServerError, {
	id: 'unexpected_server_error',
	error_contains_any: ['Unexpected server error. Check server logs for details.'],
	retryable: true,
	error_ref_pattern: 'err_[A-Fa-f0-9]{1,64}\\b',
	log_lookup: 'opencode debug paths; tail -n 200 "$HOME/.local/share/opencode/log/opencode.log"',
	fallback_action: 'OpenCode has no error-reference lookup command. Inspect the runtime log manually for <provider-error-ref>, then select another configured provider while the service is investigated.',
});
const errorRefPattern = new RegExp(unexpectedServerError.error_ref_pattern, 'g');
assert.deepEqual(
	'Unexpected server error. Check server logs for details. err_3a6d31e2 err_FACE'.match(errorRefPattern),
	['err_3a6d31e2', 'err_FACE']
);
assert.equal('Unexpected server error. Check server logs for details. err_not-hex'.match(errorRefPattern), null);
const packageJson = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'package.json'), 'utf8'));
assert.equal(packageJson.name, 'homeboy-agent-runtime-opencode');
assert.equal(packageJson.version, manifest.version);
assert.equal(packageJson.homeboy.agent_runtime_manifest, 'opencode.json');
assert.equal(JSON.stringify({ manifest, packageJson }).includes('wp-codebox'), false);
assert.equal(JSON.stringify({ manifest, packageJson }).includes('WP Codebox'), false);

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-provider-contract-'));
try {
	const executorWorkspace = path.join(root, 'executor-workspace');
	fs.mkdirSync(executorWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: executorWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '--allow-empty', '-m', 'initial'], {
		cwd: executorWorkspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});
	const runtimesRoot = path.join(root, 'agent-runtimes');
	const runtimePath = path.join(runtimesRoot, 'opencode');
	fs.mkdirSync(runtimesRoot, { recursive: true });
	fs.symlinkSync(runtimeRoot, runtimePath, 'dir');

	const [program, scriptTemplate] = provider.invocation.argv;
	assert.equal(program, 'node');
	const scriptPath = scriptTemplate.replaceAll('{{runtime_path}}', runtimePath);
	assert.equal(
		path.normalize(scriptPath),
		path.join(runtimePath, 'scripts', 'agent', 'homeboy-opencode-agent-task-executor.cjs')
	);
	assert.equal(fs.existsSync(scriptPath), true, `provider command target should exist: ${scriptPath}`);

	const mockCliPath = path.join(root, 'mock-opencode.cjs');
	fs.writeFileSync(mockCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.equal(process.argv[2], 'run');
assert.equal(process.argv.includes('--model'), false);
const instruction = process.argv.at(-1);
assert.equal([
  'Prove the OpenCode provider boundary without leaking secrets.',
  'Prove two attached runtime tools without leaking secrets.',
].includes(instruction), true);
assert.equal(process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN, 'refresh-token-must-not-leak');
assert.equal(process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN, 'access-token-must-not-leak');
assert.equal(process.env.UNDECLARED_SECRET, undefined);
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
assert.equal(config.agent.title.disable, true);
assert.equal(config.permission.external_directory['*'], 'deny');
assert.equal(config.permission.external_directory[${JSON.stringify(concretePath(executorWorkspace))}], 'allow');
assert.equal(config.agent.build.permission.external_directory['*'], 'deny');
assert.equal(config.agent.build.permission.external_directory[${JSON.stringify(concretePath(executorWorkspace))}], 'allow');
if (instruction === 'Prove two attached runtime tools without leaking secrets.') {
  assert.equal(typeof config.mcp, 'object');
  assert.equal(config.mcp['fixture.mcp'].command[0], process.execPath);
  assert.deepEqual(config.mcp['fixture.mcp'].command.slice(1), ['--fixture-mcp', '--isolated']);
  assert.equal(config.mcp['fixture.mcp'].enabled, true);
  assert.equal(config.mcp['fixture.second'].command[0], process.execPath);
  assert.deepEqual(config.mcp['fixture.second'].command.slice(1), ['--second-fixture-mcp']);
  assert.equal(config.mcp['fixture.second'].enabled, true);
}
process.stdout.write(process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN || 'missing secret');
process.stderr.write(process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN || 'missing secret');
process.exit(0);
`);

	const contractResult = spawnSync(process.execPath, [scriptPath, '--provider-contract'], { encoding: 'utf8' });
	assert.equal(contractResult.status, 0, contractResult.stderr);
	assert.deepEqual(JSON.parse(contractResult.stdout), manifest.agent_task_executors[0]);

	const request = {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'opencode-real-executor',
		executor: {
			backend: 'opencode',
			runtime: 'opencode',
			secret_env: [
				'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
				'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
			],
			config: {
				provider: 'codex',
				runtime_bin: process.execPath,
				command_args: [mockCliPath],
			},
		},
		workspace: { root: executorWorkspace },
		instructions: 'Prove the OpenCode provider boundary without leaking secrets.',
		artifacts_path: path.join(root, 'default-artifacts'),
	};
	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: {
			...process.env,
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-must-not-leak',
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-must-not-leak',
			FIXTURE_MCP_TOKEN: 'fixture-token-must-not-leak',
			UNDECLARED_SECRET: 'must-not-reach-opencode',
		},
		input: JSON.stringify({
			...request,
			instructions: 'Prove two attached runtime tools without leaking secrets.',
			resolved_runtime_tools: [fixtureRuntimeTool, secondFixtureRuntimeTool],
		}),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	const fixtureEnv = {
		...process.env,
		AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-must-not-leak',
		AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-must-not-leak',
		FIXTURE_MCP_TOKEN: 'fixture-token-must-not-leak',
		XDG_STATE_HOME: path.join(root, 'private-runtime-state'),
		UNDECLARED_SECRET: 'must-not-reach-opencode',
	};
	assert.equal(JSON.parse(runResult.stdout).status, 'succeeded');
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('refresh-token-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('access-token-must-not-leak'), false);

	const modelWorkspace = path.join(root, 'model-workspace');
	fs.mkdirSync(modelWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: modelWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '--allow-empty', '-m', 'initial'], {
		cwd: modelWorkspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});
	const realModelWorkspace = fs.realpathSync(modelWorkspace);
	const modelCliPath = path.join(root, 'mock-opencode-model.cjs');
	fs.writeFileSync(modelCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.equal(process.cwd(), ${JSON.stringify(realModelWorkspace)});
assert.equal(process.env.PWD, ${JSON.stringify(realModelWorkspace)});
assert.deepEqual(process.argv.slice(2, 9), ['run', '--format', 'json', '--model', 'opencode-go/kimi-k2.7-code', '--dir', ${JSON.stringify(realModelWorkspace)}]);
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
assert.equal(config.$schema, 'https://opencode.ai/config.json');
assert.equal(config.model, 'opencode-go/kimi-k2.7-code');
assert.equal(config.agent.build.model, 'opencode-go/kimi-k2.7-code');
assert.equal(config.small_model, 'zai-coding-plan/glm-5.2');
assert.equal(config.agent.title.disable, true);
	assert.equal(config.agent.title.model, 'ambient-title-model-must-not-change');
	assert.deepEqual(config.mcp, { example: { type: 'local' } });
	assert.equal(Object.hasOwn(config, 'agents'), false);
	assert.deepEqual(config.permission, {
	  read: {
	    '*': 'allow',
	    '..': 'deny',
	    '../*': 'deny',
	    '..\\\\*': 'deny',
	    '*.env': 'deny'
	  },
	  glob: { '*': 'allow' },
	  grep: { '*': 'allow', 'secret': 'deny' },
	  edit: { '*': 'allow' },
	  bash: { '*': 'allow', 'git push *': 'deny' },
	  external_directory: {
	    '*': 'deny',
	    '/unrelated/**': 'deny',
	    ${JSON.stringify(realModelWorkspace)}: 'allow',
	    ${JSON.stringify(path.join(realModelWorkspace, '**'))}: 'allow'
	  }
	});
	assert.deepEqual(config.agent.build.permission, {
	  read: {
	    '*': 'allow',
	    '..': 'deny',
	    '../*': 'deny',
	    '..\\\\*': 'deny',
	    '*.env': 'deny'
	  },
	  glob: { '*': 'allow' },
	  grep: { '*': 'allow', 'secret': 'deny' },
	  edit: { '*': 'allow' },
	  bash: { '*': 'allow', 'git push *': 'deny' },
	  external_directory: {
	    '*': 'deny',
	    ${JSON.stringify(realModelWorkspace)}: 'allow',
	    ${JSON.stringify(path.join(realModelWorkspace, '**'))}: 'allow'
	  }
	});
	process.exit(0);
`);
	const modelResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-executor-model',
		executor: {
			...request.executor,
			model: 'opencode-go/kimi-k2.7-code',
			config: {
				...request.executor.config,
				command_args: [modelCliPath],
				workspace_root: modelWorkspace,
				runtime_env: {
					OPENCODE_CONFIG_CONTENT: JSON.stringify({
						mcp: { example: { type: 'local' } },
						permission: {
							read: { '*.env': 'deny' },
							glob: { '*': 'ask' },
							grep: { '*': 'ask', secret: 'deny' },
							edit: { '*': 'ask' },
							bash: { '*': 'ask', 'git push *': 'deny' },
							external_directory: { '/unrelated/**': 'deny' },
						},
						agents: { build: { model: 'invalid-plural-key/must-not-survive' } },
						agent: { title: { disable: false, model: 'ambient-title-model-must-not-change' } },
					}),
				},
				small_model: 'zai-coding-plan/glm-5.2',
			},
		},
	}, { env: fixtureEnv });
	assert.equal(modelResult.status, 'succeeded', JSON.stringify(modelResult.diagnostics));

	const permissionWorkspaces = [
		{
			label: 'relative-long-attempt-workspace',
			attemptRoot: path.join(root, 'controller-scratch', 'cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874'),
			workspace: path.join(root, 'controller-scratch', 'cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874', 'workspace'),
			workspacePermissionRoot: path.join(root, 'controller-scratch', 'cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874', 'workspace'),
			workspaceConfig: 'relative-cwd',
			allowAttemptRoot: true,
			workspaceSiblingAction: 'allow',
		},
		{
			label: 'canonical-permission-root',
			workspace: path.join(root, 'canonical-permission-root', 'workspace'),
			workspacePermissionRoot: path.join(root, 'canonical-permission-root'),
			workspaceConfig: 'cwd',
			workspaceSiblingAction: 'allow',
		},
		{
			label: 'unrelated-attempt-root',
			attemptRoot: path.join(root, 'unrelated-attempt-root'),
			workspace: path.join(root, 'unrelated-workspace'),
			workspaceConfig: 'cwd',
			allowAttemptRoot: false,
		},
		{
			label: 'prefix-collision-attempt-root',
			attemptRoot: path.join(root, 'controller-scratch', 'wp-codebox-1825-gate-fix-2d'),
			workspace: path.join(root, 'controller-scratch', 'wp-codebox-1825-gate-fix-2d-sibling', 'workspace'),
			workspaceConfig: 'cwd',
			allowAttemptRoot: false,
		},
		{
			label: 'managed-worktree',
			workspace: path.join(root, 'managed-worktrees', 'homeboy-extensions@attempt'),
			workspaceConfig: 'workspace_path',
		},
	];
	const ambientTmpdir = path.join(root, 'ambient-process-tmpdir');
	for (const permissionWorkspace of permissionWorkspaces) {
		fs.mkdirSync(permissionWorkspace.workspace, { recursive: true });
		if (permissionWorkspace.workspacePermissionRoot) {
			fs.mkdirSync(permissionWorkspace.workspacePermissionRoot, { recursive: true });
		}
		spawnSync('git', ['init'], { cwd: permissionWorkspace.workspace, encoding: 'utf8' });
		spawnSync('git', ['commit', '--allow-empty', '-m', 'initial'], {
			cwd: permissionWorkspace.workspace,
			encoding: 'utf8',
			env: {
				...process.env,
				GIT_AUTHOR_NAME: 'Homeboy Test',
				GIT_AUTHOR_EMAIL: 'homeboy@example.test',
				GIT_COMMITTER_NAME: 'Homeboy Test',
				GIT_COMMITTER_EMAIL: 'homeboy@example.test',
			},
		});
		const concreteWorkspace = concretePath(permissionWorkspace.workspace);
		const concretePermissionRoot = concretePath(permissionWorkspace.workspacePermissionRoot || permissionWorkspace.workspace);
		const workspacePatterns = [...new Set([
			concretePermissionRoot,
			path.join(concretePermissionRoot, '**'),
			concreteWorkspace,
			path.join(concreteWorkspace, '**'),
		])];
		const concreteAttemptRoot = permissionWorkspace.attemptRoot && concretePath(permissionWorkspace.attemptRoot);
		const attemptRootPattern = concreteAttemptRoot && path.join(concreteAttemptRoot, '*');
		const expectedAttemptRootPattern = permissionWorkspace.allowAttemptRoot ? attemptRootPattern : undefined;
		const configCapturePath = path.join(root, `opencode-permission-${permissionWorkspace.label}.json`);
		const permissionCliPath = path.join(root, `mock-opencode-permission-${permissionWorkspace.label}.cjs`);
		fs.writeFileSync(permissionCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
assert.equal(process.cwd(), ${JSON.stringify(concreteWorkspace)});
assert.equal(process.argv.at(-3), '--dir');
assert.equal(process.argv.at(-2), ${JSON.stringify(concreteWorkspace)});
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
for (const pattern of ${JSON.stringify(workspacePatterns)}) {
  assert.equal(config.permission.external_directory[pattern], 'allow');
  assert.equal(config.agent.build.permission.external_directory[pattern], 'allow');
}
assert.equal(config.permission.external_directory[${JSON.stringify(attemptRootPattern)}], ${JSON.stringify(expectedAttemptRootPattern ? 'allow' : undefined)});
assert.equal(config.permission.external_directory[${JSON.stringify(path.join(ambientTmpdir, '*'))}], undefined);
assert.equal(config.permission.external_directory['/unrelated/**'], 'deny');
assert.equal(config.permission.external_directory['*'], 'deny');
assert.deepEqual(config.permission.grep, { '*': 'allow', secret: 'deny' });
assert.deepEqual(config.permission.glob, { '*': 'allow' });
		assert.deepEqual(config.permission.read, {
			'*': 'allow',
			'..': 'deny',
			'../*': 'deny',
			'..\\\\*': 'deny',
			'*.env': 'deny',
		});
		assert.deepEqual(config.permission.edit, { '*': 'allow' });
		assert.deepEqual(config.permission.bash, { '*': 'allow', 'git push *': 'deny' });
		assert.deepEqual(config.agent.build.permission.read, config.permission.read);
		assert.deepEqual(config.agent.build.permission.glob, config.permission.glob);
		assert.deepEqual(config.agent.build.permission.grep, config.permission.grep);
		assert.deepEqual(config.agent.build.permission.edit, config.permission.edit);
		assert.deepEqual(config.agent.build.permission.bash, config.permission.bash);
assert.equal(config.agent.build.permission.external_directory[${JSON.stringify(attemptRootPattern)}], ${JSON.stringify(expectedAttemptRootPattern ? 'allow' : undefined)});
fs.writeFileSync(${JSON.stringify(configCapturePath)}, JSON.stringify(config));
process.exit(0);
`);
		const workspaceRequest = {
			...request,
			task_id: `opencode-permission-${permissionWorkspace.label}`,
			policy: { write: 'patch' },
			executor: {
				...request.executor,
				config: {
					...request.executor.config,
					command_args: [permissionCliPath],
					runtime_env: {
						OPENCODE_CONFIG_CONTENT: JSON.stringify({
							permission: {
								external_directory: { '*': 'deny', '/unrelated/**': 'deny' },
								grep: { '*': 'ask', secret: 'deny' }, glob: { '*': 'ask' }, read: { '*.env': 'deny' }, edit: { '*': 'ask' }, bash: { '*': 'ask', 'git push *': 'deny' },
							},
						}),
					},
				},
			},
		};
		if (permissionWorkspace.workspaceConfig === 'cwd') {
			workspaceRequest.executor.config.cwd = permissionWorkspace.workspace;
		} else if (permissionWorkspace.workspaceConfig === 'relative-cwd') {
			workspaceRequest.executor.config.cwd = path.relative(process.cwd(), permissionWorkspace.workspace);
			assert.equal(path.isAbsolute(workspaceRequest.executor.config.cwd), false);
		} else {
			workspaceRequest.workspace_path = permissionWorkspace.workspace;
		}
		if (permissionWorkspace.workspacePermissionRoot) {
			workspaceRequest.executor.config.workspace_permission_root = permissionWorkspace.workspacePermissionRoot;
		}
		if (permissionWorkspace.attemptRoot) {
			workspaceRequest.executor.config.runtime_env.TMPDIR = permissionWorkspace.attemptRoot;
		}
		const permissionResult = await executeOpenCodeAgentTask(workspaceRequest, {
			env: { ...fixtureEnv, TMPDIR: ambientTmpdir },
		});
		assert.equal(permissionResult.status, 'succeeded', JSON.stringify(permissionResult.diagnostics));
		const generatedConfig = JSON.parse(fs.readFileSync(configCapturePath, 'utf8'));
		const requestedPatterns = permissionWorkspace.allowAttemptRoot
			? [
				path.join(concreteAttemptRoot, '*'),
				path.join(concreteAttemptRoot, 'workspace', 'packages', 'runtime-playground', 'src', '*'),
				path.join(concreteAttemptRoot, 'workspace', 'tests', '*'),
				path.join(concreteWorkspace, 'packages', 'runtime-playground', 'src', '*'),
				path.join(concreteWorkspace, 'tests', '*'),
			]
			: [path.join(concreteWorkspace, 'tests', '*')];
		for (const requestedPattern of requestedPatterns) {
			assert.equal(externalDirectoryAction(generatedConfig, 'build', requestedPattern), 'allow');
		}
		assert.equal(
			readAction(generatedConfig, 'build', concreteWorkspace, path.join(concreteWorkspace, 'src', 'index.js')),
			'allow'
		);
		assert.equal(nativeToolAction(generatedConfig, 'build', 'glob', 'src/**/*.js'), 'allow');
		assert.equal(nativeToolAction(generatedConfig, 'build', 'grep', 'TODO'), 'allow');
		assert.equal(nativeToolAction(generatedConfig, 'build', 'grep', 'secret'), 'deny');
		assert.equal(nativeToolAction(generatedConfig, 'build', 'edit', 'src/index.js'), 'allow');
		assert.equal(nativeToolAction(generatedConfig, 'build', 'bash', 'git status --short'), 'allow');
		assert.equal(nativeToolAction(generatedConfig, 'build', 'bash', 'git push origin trunk'), 'deny');
		for (const tool of ['read', 'glob', 'grep', 'edit', 'bash']) {
			assert.equal(nativeToolAction(generatedConfig, 'build', tool, 'src/index.js'), 'allow');
		}
		assert.equal(externalDirectoryAction(generatedConfig, 'build', concreteWorkspace), 'allow');
		assert.equal(externalDirectoryAction(generatedConfig, 'build', path.join(concreteWorkspace, 'src', 'index.js')), 'allow');
		assert.equal(
			externalDirectoryAction(generatedConfig, 'build', `${concreteWorkspace}-sibling`),
			permissionWorkspace.workspaceSiblingAction || 'deny'
		);
		assert.equal(externalDirectoryAction(generatedConfig, 'build', path.join(root, 'controller-scratch')), 'deny');
		assert.equal(
			readAction(generatedConfig, 'build', concreteWorkspace, path.join(root, 'outside-workspace', 'index.js')),
			'deny'
		);
		if (permissionWorkspace.attemptRoot && !permissionWorkspace.allowAttemptRoot) {
			assert.equal(
				externalDirectoryAction(generatedConfig, 'build', path.join(concreteAttemptRoot, '*')),
				'deny'
			);
			assert.equal(
				externalDirectoryAction(generatedConfig, 'build', path.join(concreteAttemptRoot, 'workspace', 'tests', '*')),
				'deny'
			);
		}
		assert.equal(
			externalDirectoryAction(generatedConfig, 'build', path.join(root, 'controller-scratch', 'unrelated-attempt', '*')),
			'deny'
		);
		if (permissionWorkspace.workspacePermissionRoot) {
			assert.equal(
				externalDirectoryAction(generatedConfig, 'build', path.join(concreteWorkspace, '**')),
				'allow'
			);
		}
	}

	const resolverWorkspace = concretePath(permissionWorkspaces[0].workspace);
	const resolverFile = path.join(resolverWorkspace, 'crates', 'opencode-resolver-fixture', 'src', 'lib.rs');
	const resolverRelativeFile = 'crates/opencode-resolver-fixture/src/lib.rs';
	const resolverContent = 'pub const OPENCODE_RESOLVER_FIXTURE: &str = "workspace-relative";\n';
	fs.mkdirSync(path.dirname(resolverFile), { recursive: true });
	fs.writeFileSync(resolverFile, resolverContent);
	const opencode = installedOpenCodeBinary();
	if (opencode) {
		const search = spawnSync(opencode, ['debug', 'file', 'search', 'opencode-resolver-fixture', '--pure'], {
			cwd: resolverWorkspace,
			encoding: 'utf8',
		});
		assert.equal(search.status, 0, search.stderr);
		const searchResults = search.stdout.trim().split(/\r?\n/).filter(Boolean);
		assert.equal(searchResults.includes(resolverRelativeFile), true, search.stdout);
		assert.equal(searchResults.some((result) => path.isAbsolute(result) || result === '..' || result.startsWith('../')), false, search.stdout);

		const read = spawnSync(opencode, ['debug', 'file', 'read', resolverRelativeFile, '--pure'], {
			cwd: resolverWorkspace,
			encoding: 'utf8',
		});
		assert.equal(read.status, 0, read.stderr);
		const resolvedFile = JSON.parse(read.stdout);
		assert.equal(Buffer.from(resolvedFile.content, resolvedFile.encoding).toString('utf8'), resolverContent);

		const outsideFile = path.join(path.dirname(resolverWorkspace), 'opencode-resolver-outside.txt');
		fs.writeFileSync(outsideFile, 'outside workspace');
		const escapedRead = spawnSync(opencode, ['debug', 'file', 'read', '../opencode-resolver-outside.txt', '--pure'], {
			cwd: resolverWorkspace,
			encoding: 'utf8',
		});
		assert.notEqual(escapedRead.status, 0, 'OpenCode file resolver escaped the configured workspace root.');
	}

	const preflightWorkspace = path.join(root, 'controller-scratch', 'cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874', 'workspace');
	fs.mkdirSync(preflightWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: preflightWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '--allow-empty', '-m', 'initial'], {
		cwd: preflightWorkspace,
		encoding: 'utf8',
		env: { ...process.env, GIT_AUTHOR_NAME: 'Homeboy Test', GIT_AUTHOR_EMAIL: 'homeboy@example.test', GIT_COMMITTER_NAME: 'Homeboy Test', GIT_COMMITTER_EMAIL: 'homeboy@example.test' },
	});
	const concretePreflightWorkspace = concretePath(preflightWorkspace);
	const preflightOpenCode = path.join(root, 'opencode');
	const preflightRunMarker = path.join(root, 'opencode-preflight-run-marker');
	fs.writeFileSync(preflightOpenCode, `#!/usr/bin/env node
const fs = require('node:fs');
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
const permission = Object.entries(config.agent.build.permission).flatMap(([name, rules]) => Object.entries(rules).map(([pattern, action]) => ({ permission: name, pattern, action })));
if (process.argv[2] === 'debug') {
  const omitted = process.env.HOMEBOY_TEST_OMIT_WORKSPACE_ROOT;
  const omittedPermission = process.env.HOMEBOY_TEST_OMIT_PERMISSION;
  process.stdout.write(JSON.stringify({ permission: permission.filter((rule) => !(omitted && rule.permission === 'external_directory' && rule.pattern === omitted) && rule.permission !== omittedPermission) }));
  process.exit(0);
}
if (process.argv[2] === 'run') fs.appendFileSync(${JSON.stringify(preflightRunMarker)}, 'run\\n');
process.exit(0);
`);
	fs.chmodSync(preflightOpenCode, 0o755);
	const preflightRequest = (taskId, policy = { write: 'patch' }, runtimeEnv = {}) => ({
		...request,
		task_id: taskId,
		policy,
		workspace_path: preflightWorkspace,
		executor: { ...request.executor, config: { ...request.executor.config, runtime_bin: preflightOpenCode, command_args: [], runtime_env: runtimeEnv } },
	});
	const preflightFailure = await executeOpenCodeAgentTask(preflightRequest('opencode-workspace-preflight-failure', { write: 'patch' }, { HOMEBOY_TEST_OMIT_WORKSPACE_ROOT: concretePreflightWorkspace }), { env: fixtureEnv });
	assert.equal(preflightFailure.status, 'provider_error');
	assert.equal(preflightFailure.failure_code, 'agent_task.opencode_workspace_permission_preflight_failed');
	assert.equal(preflightFailure.diagnostics[0].class, 'opencode.workspace_permission_preflight');
	assert.equal(fs.existsSync(preflightRunMarker), false);
	const preflightReadOnly = await executeOpenCodeAgentTask(preflightRequest('opencode-workspace-preflight-readonly', { write: 'none' }, { HOMEBOY_TEST_OMIT_PERMISSION: 'edit' }), { env: fixtureEnv });
	assert.equal(preflightReadOnly.status, 'succeeded', JSON.stringify(preflightReadOnly.diagnostics));
	const preflightReadWriteMissingEdit = await executeOpenCodeAgentTask(preflightRequest('opencode-workspace-preflight-readwrite-missing-edit', { write: 'patch' }, { HOMEBOY_TEST_OMIT_PERMISSION: 'edit' }), { env: fixtureEnv });
	assert.equal(preflightReadWriteMissingEdit.failure_code, 'agent_task.opencode_workspace_permission_preflight_failed');
	const preflightReadWrite = await executeOpenCodeAgentTask(preflightRequest('opencode-workspace-preflight-readwrite'), { env: fixtureEnv });
	assert.equal(preflightReadWrite.status, 'succeeded', JSON.stringify(preflightReadWrite.diagnostics));
	assert.equal(fs.readFileSync(preflightRunMarker, 'utf8'), 'run\nrun\n');

	const provenanceOpenCode = path.join(root, 'opencode');
	fs.writeFileSync(provenanceOpenCode, `#!/usr/bin/env node
const fs = require('node:fs');
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
if (process.argv[2] === 'debug') {
  const permission = Object.entries(config.agent.build.permission)
    .flatMap(([name, rules]) => Object.entries(rules).map(([pattern, action]) => ({ permission: name, pattern, action })));
  process.stdout.write(JSON.stringify({ permission }));
  process.exit(0);
}
if (process.argv[2] === 'export') {
  if (process.env.HOMEBOY_TEST_HANG_EXPORT === '1') {
    setInterval(() => {}, 1_000);
  } else {
	if (process.env.HOMEBOY_TEST_DELAYED_EXPORT === '1') {
	  const marker = ${JSON.stringify(path.join(root, 'delayed-session-export-marker'))};
	  if (!fs.existsSync(marker)) {
	    fs.writeFileSync(marker, 'first export unavailable');
	    process.exit(1);
	  }
	}
    const exported = 'Exporting session\\n' + JSON.stringify({
      info: { model: { providerID: 'openai', id: 'gpt-5.6-sol' } },
      messages: [{ text: 'x'.repeat(70 * 1024) }],
    });
    process.stdout.write(fs.fstatSync(1).isFile() ? exported : exported.slice(0, 64 * 1024));
    process.exit(0);
  }
} else {
  process.stdout.write(JSON.stringify({ type: 'step_start', sessionID: 'ses_default_model', part: { sessionID: 'ses_default_model' } }) + '\\n');
  if (process.env.HOMEBOY_TEST_CONFLICT_SESSION === '1') {
    process.stdout.write(JSON.stringify({ type: 'step_end', sessionID: 'ses_conflicting_model' }) + '\\n');
  }
  process.exit(0);
}
`);
	fs.chmodSync(provenanceOpenCode, 0o755);
	const provenanceTempRoot = path.join(root, 'provenance-temp');
	fs.mkdirSync(provenanceTempRoot);
	const defaultModelRun = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: fixtureEnv,
		input: JSON.stringify({
			...request,
			task_id: 'opencode-default-model-provenance',
			workspace_path: preflightWorkspace,
			artifacts_path: path.join(root, 'default-model-artifacts'),
			executor: {
				...request.executor,
				config: { ...request.executor.config, runtime_bin: provenanceOpenCode, command_args: [], runtime_env: { TMPDIR: provenanceTempRoot } },
			},
		}),
	});
	assert.equal(defaultModelRun.status, 0, defaultModelRun.stderr);
	const defaultModelResult = JSON.parse(defaultModelRun.stdout);
	assert.equal(defaultModelResult.schema, 'homeboy/agent-task-outcome/v1');
	assert.equal(defaultModelResult.task_id, 'opencode-default-model-provenance');
	assert.equal(defaultModelResult.status, 'succeeded', JSON.stringify(defaultModelResult.diagnostics));
	assert.equal(defaultModelResult.metadata.model, 'openai/gpt-5.6-sol');
	assert.deepEqual(defaultModelResult.metadata.opencode_session, {
		status: 'captured', session_id: 'ses_default_model', model: 'openai/gpt-5.6-sol',
	});
	assert.deepEqual(fs.readdirSync(provenanceTempRoot).filter((name) => name.startsWith('opencode-session-export-')), []);
	const delayedModelResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-delayed-session-model',
		workspace_path: preflightWorkspace,
		artifacts_path: path.join(root, 'delayed-model-artifacts'),
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				runtime_bin: provenanceOpenCode,
				command_args: [],
				runtime_env: { HOMEBOY_TEST_DELAYED_EXPORT: '1' },
			},
		},
	}, { env: fixtureEnv });
	assert.equal(delayedModelResult.status, 'succeeded', JSON.stringify(delayedModelResult.diagnostics));
	assert.equal(delayedModelResult.metadata.model, 'openai/gpt-5.6-sol');
	assert.deepEqual(delayedModelResult.metadata.opencode_session, {
		status: 'captured', session_id: 'ses_default_model', model: 'openai/gpt-5.6-sol',
	});
	const exportStarted = Date.now();
	const unavailableModelResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-unavailable-session-model',
		workspace_path: preflightWorkspace,
		artifacts_path: path.join(root, 'unavailable-model-artifacts'),
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				runtime_bin: provenanceOpenCode,
				command_args: [],
				runtime_env: { HOMEBOY_TEST_HANG_EXPORT: '1' },
			},
		},
	}, { env: fixtureEnv });
	assert.ok(Date.now() - exportStarted < 5_000, 'session export retries must not block provider completion');
	assert.equal(unavailableModelResult.status, 'succeeded', JSON.stringify(unavailableModelResult.diagnostics));
	assert.deepEqual(unavailableModelResult.metadata.opencode_session, {
		status: 'unavailable',
		session_id: 'ses_default_model',
		reason: 'OpenCode did not return a readable completed-session export.',
	});
	assert.equal(unavailableModelResult.metadata.model, undefined);
	const conflictingSessionResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-conflicting-session-provenance',
		workspace_path: preflightWorkspace,
		artifacts_path: path.join(root, 'conflicting-session-artifacts'),
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				runtime_bin: provenanceOpenCode,
				command_args: [],
				runtime_env: { HOMEBOY_TEST_CONFLICT_SESSION: '1' },
			},
		},
	}, { env: fixtureEnv });
	assert.equal(conflictingSessionResult.status, 'succeeded', JSON.stringify(conflictingSessionResult.diagnostics));
	assert.deepEqual(conflictingSessionResult.metadata.opencode_session, {
		status: 'unavailable',
		reason: 'OpenCode completion output contained conflicting session identifiers.',
	});
	assert.equal(conflictingSessionResult.metadata.model, undefined);

	const scratchAttempts = [
		{ id: 'run-2250-attempt-1', status: 0 },
		{ id: 'run-2250-attempt-2', status: 17 },
	];
	for (const attempt of scratchAttempts) {
		const scratchRoot = path.join(root, 'scratch', attempt.id);
		fs.mkdirSync(scratchRoot, { recursive: true });
		const scratchCliPath = path.join(root, `mock-opencode-scratch-${attempt.status}.cjs`);
		fs.writeFileSync(scratchCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
assert.equal(process.env.TMPDIR, ${JSON.stringify(scratchRoot)});
assert.equal(process.env.UNRELATED_RUNTIME_ENV, 'preserved-for-provider');
fs.writeFileSync(path.join(process.env.TMPDIR, 'provider-scratch.txt'), process.env.TMPDIR);
process.exit(${attempt.status});
`);
		const scratchResult = await executeOpenCodeAgentTask({
			...request,
			task_id: attempt.id,
			executor: {
				...request.executor,
				config: {
					...request.executor.config,
					command_args: [scratchCliPath],
					runtime_env: {
						TMPDIR: scratchRoot,
						UNRELATED_RUNTIME_ENV: 'preserved-for-provider',
					},
				},
			},
		}, { env: fixtureEnv });
		assert.equal(scratchResult.status, attempt.status === 0 ? 'succeeded' : 'failed');
		assert.equal(fs.readFileSync(path.join(scratchRoot, 'provider-scratch.txt'), 'utf8'), scratchRoot);
		const marker = JSON.parse(fs.readFileSync(path.join(scratchRoot, '.homeboy-opencode-retention.json'), 'utf8'));
		assert.equal(marker.schema, 'homeboy/opencode-retention-marker/v1');
		assert.equal(marker.task_id, attempt.id);
		assert.equal(marker.active, false);
		assert.match(marker.signature, /^[a-f0-9]{64}$/);
	}
	assert.equal(
		fs.existsSync(path.join(root, 'scratch', 'run-2250-attempt-1', 'provider-scratch.txt')),
		true
	);
	assert.equal(
		fs.existsSync(path.join(root, 'scratch', 'run-2250-attempt-2', 'provider-scratch.txt')),
		true
	);

	const missingArtifactResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-missing-artifact',
		expected_artifacts: ['opencode-report'],
	}, { env: fixtureEnv });
	assert.equal(missingArtifactResult.status, 'failed');
	assert.equal(missingArtifactResult.failure_code, 'agent_task.opencode_missing_declared_artifacts');
	assert.match(missingArtifactResult.summary, /opencode-report/);
	assert.equal(missingArtifactResult.metadata.missing_declared_artifacts[0].name, 'opencode-report');

	const workspace = path.join(root, 'workspace');
	const artifactDir = path.join(root, 'artifacts');
	fs.mkdirSync(workspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: workspace, encoding: 'utf8' });
	fs.writeFileSync(path.join(workspace, 'README.md'), 'before\n');
	fs.writeFileSync(path.join(workspace, 'UNSTAGED.md'), 'before unstaged\n');
	spawnSync('git', ['add', 'README.md', 'UNSTAGED.md'], { cwd: workspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '-m', 'initial'], {
		cwd: workspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});

	const artifactCliPath = path.join(root, 'mock-opencode-artifact.cjs');
	fs.writeFileSync(artifactCliPath, `#!/usr/bin/env node
	const fs = require('node:fs');
	const { spawnSync } = require('node:child_process');
	fs.writeFileSync('README.md', 'committed after\\n');
	spawnSync('git', ['add', 'README.md']);
	spawnSync('git', ['-c', 'user.name=Homeboy Test', '-c', 'user.email=homeboy@example.test', 'commit', '-m', 'agent commit']);
	fs.writeFileSync('STAGED.md', 'staged after\\n');
	spawnSync('git', ['add', 'STAGED.md']);
	fs.writeFileSync('UNSTAGED.md', 'unstaged after\\n');
	fs.writeFileSync('NEW.md', 'untracked\\n');
	fs.writeFileSync('BINARY.bin', Buffer.from([0, 1, 2, 0, 255]));
	process.stdout.write('transcript output ' + process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN);
	process.stderr.write('provider stderr output');
	process.exit(0);
`);
	const artifactResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-artifacts',
		workspace_path: workspace,
		artifacts_path: artifactDir,
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [artifactCliPath],
			},
		},
	}, { env: fixtureEnv });
	assert.equal(artifactResult.status, 'succeeded');
	assert.deepEqual(artifactResult.artifacts.map((artifact) => artifact.name).sort(), ['agent_result', 'opencode-runtime-stderr', 'opencode-runtime-stdout', 'patch', 'transcript']);
	const editingPatch = fs.readFileSync(artifactResult.artifacts.find((artifact) => artifact.name === 'patch').path, 'utf8');
	assert.match(editingPatch, /committed after/);
	assert.equal(editingPatch.match(/committed after/g)?.length, 1);
	assert.match(editingPatch, /STAGED\.md/);
	assert.match(editingPatch, /staged after/);
	assert.match(editingPatch, /unstaged after/);
	assert.match(editingPatch, /NEW\.md/);
	assert.match(editingPatch, /untracked/);
	assert.match(editingPatch, /BINARY\.bin/);
	assert.match(editingPatch, /GIT binary patch/);
	const transcript = fs.readFileSync(artifactResult.artifacts.find((artifact) => artifact.name === 'transcript').path, 'utf8');
	assert.match(transcript, /transcript output/);
	assert.match(transcript, /provider stderr output/);
	assert.equal(transcript.includes('refresh-token-must-not-leak'), false);
	assert.match(transcript, /\[redacted\]/);
	assert.equal(artifactResult.artifacts.some((artifact) => artifact.name === 'opencode-runtime-stdout'), true);
	assert.equal(artifactResult.artifacts.every((artifact) => artifact.schema === 'homeboy/agent-task-artifact/v1' && artifact.url === artifact.uri && Number.isInteger(artifact.size_bytes)), true);
	assert.equal(artifactResult.evidence_refs.every((ref) => ref.uri.startsWith('file://')), true);
	assert.equal(artifactResult.evidence_refs.some((ref) => Object.hasOwn(ref, 'path')), false);
	const runtimeStdout = fs.readFileSync(artifactResult.artifacts.find((artifact) => artifact.name === 'opencode-runtime-stdout').path, 'utf8');
	assert.match(runtimeStdout, /transcript output/);
	assert.equal(runtimeStdout.includes('refresh-token-must-not-leak'), false);
	assert.match(runtimeStdout, /\[redacted\]/);
	assert.equal(artifactResult.metadata.opencode_session.status, 'not_discovered');
	const agentResult = JSON.parse(fs.readFileSync(artifactResult.artifacts.find((artifact) => artifact.name === 'agent_result').path, 'utf8'));
	assert.equal(agentResult.opencode_session.status, 'not_discovered');
	assert.equal(artifactResult.metadata.missing_declared_artifacts, undefined);

	const declaredArtifactCliPath = path.join(root, 'mock-opencode-declared-artifacts.cjs');
	fs.writeFileSync(declaredArtifactCliPath, `#!/usr/bin/env node
const fs = require('node:fs');
fs.mkdirSync('declared-screenshots', { recursive: true });
fs.writeFileSync('declared-report.md', '# OpenCode report\\n');
fs.writeFileSync('declared-screenshots/image.bin', Buffer.from([0, 255, 1, 254]));
process.exit(0);
`);
	const noOpDeclaredArtifactCliPath = path.join(root, 'mock-opencode-no-op-declared-artifacts.cjs');
	fs.writeFileSync(noOpDeclaredArtifactCliPath, '#!/usr/bin/env node\nprocess.exit(0);\n');
	const declaredOpenCodeResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-declared-artifacts',
		workspace_path: workspace,
		artifacts_path: path.join(root, 'declared-opencode-artifacts'),
		artifact_declarations: [
			{ name: 'report', path: 'declared-report.md', kind: 'markdown', artifact_type: 'report', artifact_schema: 'example/report/v1', required: true, metadata: { source: 'opencode' } },
			{ name: 'screenshots', path: 'declared-screenshots', kind: 'screenshot-directory', required: true },
			{ name: 'optional-video', path: 'missing.webm', kind: 'video', required: false },
		],
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [declaredArtifactCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(declaredOpenCodeResult.status, 'succeeded', JSON.stringify(declaredOpenCodeResult.diagnostics));
	const declaredReport = declaredOpenCodeResult.artifacts.find((artifact) => artifact.name === 'report');
	const declaredScreenshots = declaredOpenCodeResult.artifacts.find((artifact) => artifact.name === 'screenshots');
	assert.equal(declaredReport.artifact_schema, 'example/report/v1');
	assert.equal(declaredReport.artifact_type, 'report');
	assert.equal(declaredReport.metadata.source, 'opencode');
	assert.equal(declaredReport.bytes, Buffer.byteLength('# OpenCode report\n'));
	assert.equal(declaredReport.schema, 'homeboy/agent-task-artifact/v1');
	assert.equal(declaredReport.url, declaredReport.uri);
	assert.equal(declaredReport.size_bytes, Buffer.byteLength('# OpenCode report\n'));
	assert.match(declaredReport.sha256, /^[a-f0-9]{64}$/);
	assert.deepEqual(fs.readFileSync(path.join(declaredScreenshots.path, 'image.bin')), Buffer.from([0, 255, 1, 254]));
	assert.equal(declaredOpenCodeResult.diagnostics.some((diagnostic) => diagnostic.class === 'agent_task.optional_declared_artifact_missing'), true);
	const unsafeOpenCodeResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-unsafe-declared-artifact',
		workspace_path: workspace,
		artifacts_path: path.join(root, 'unsafe-opencode-artifacts'),
		artifact_declarations: [{ name: 'unsafe', path: '../outside.md', required: true }],
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [noOpDeclaredArtifactCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(unsafeOpenCodeResult.status, 'failed');
	assert.equal(unsafeOpenCodeResult.diagnostics.some((diagnostic) => diagnostic.class === 'agent_task.declared_artifact_unsafe_path'), true);
	const missingPathOpenCodeResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-missing-path-declared-artifact',
		workspace_path: workspace,
		artifacts_path: path.join(root, 'missing-path-opencode-artifacts'),
		artifact_declarations: [{ name: 'missing-path', required: true }],
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [noOpDeclaredArtifactCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(missingPathOpenCodeResult.status, 'succeeded');
	assert.equal(missingPathOpenCodeResult.diagnostics.some((diagnostic) => diagnostic.class === 'agent_task.declared_artifact_invalid_path'), false);
	assert.equal(missingPathOpenCodeResult.artifacts.some((artifact) => artifact.name === 'missing-path'), false);

	const largePatchCliPath = path.join(root, 'mock-opencode-large-patch.cjs');
	fs.writeFileSync(largePatchCliPath, `#!/usr/bin/env node
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
fs.writeFileSync('LARGE.md', 'x'.repeat(2 * 1024 * 1024) + '\\n');
spawnSync('git', ['add', 'LARGE.md']);
process.exit(0);
`);
	const largePatchResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-large-patch-artifact',
		workspace_path: workspace,
		artifacts_path: path.join(root, 'large-patch-artifacts'),
		expected_artifacts: ['patch'],
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [largePatchCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(largePatchResult.status, 'succeeded');
	const largePatch = fs.readFileSync(largePatchResult.artifacts.find((artifact) => artifact.name === 'patch').path, 'utf8');
	const largePatchContent = 'x'.repeat(2 * 1024 * 1024);
	assert.ok(Buffer.byteLength(largePatch) > 1024 * 1024);
	assert.equal(largePatch.includes(`+${largePatchContent}`), true);

	const committedWorkspace = path.join(root, 'committed-workspace');
	fs.mkdirSync(committedWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: committedWorkspace, encoding: 'utf8' });
	fs.writeFileSync(path.join(committedWorkspace, 'README.md'), 'before commit\n');
	spawnSync('git', ['add', 'README.md'], { cwd: committedWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '-m', 'initial'], {
		cwd: committedWorkspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});
	const committedCliPath = path.join(root, 'mock-opencode-committed.cjs');
	fs.writeFileSync(committedCliPath, `#!/usr/bin/env node
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
fs.writeFileSync('README.md', 'committed only\\n');
spawnSync('git', ['add', 'README.md']);
spawnSync('git', ['-c', 'user.name=Homeboy Test', '-c', 'user.email=homeboy@example.test', 'commit', '-m', 'agent commit']);
process.exit(0);
`);
	const committedResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-committed-artifacts',
		workspace_path: committedWorkspace,
		artifacts_path: path.join(root, 'committed-artifacts'),
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [committedCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(committedResult.status, 'succeeded');
	const committedPatch = fs.readFileSync(committedResult.artifacts.find((artifact) => artifact.name === 'patch').path, 'utf8');
	assert.match(committedPatch, /committed only/);
	assert.equal(committedPatch.match(/committed only/g)?.length, 1);

	const quietWorkspace = path.join(root, 'quiet-workspace');
	fs.mkdirSync(quietWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: quietWorkspace, encoding: 'utf8' });
	fs.writeFileSync(path.join(quietWorkspace, 'README.md'), 'unchanged\n');
	spawnSync('git', ['add', 'README.md'], { cwd: quietWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '-m', 'initial'], {
		cwd: quietWorkspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});

	const quietCliPath = path.join(root, 'mock-opencode-quiet.cjs');
	fs.writeFileSync(quietCliPath, `#!/usr/bin/env node
process.exit(0);
`);
	const quietArtifactDir = path.join(root, 'quiet-artifacts');
	const quietResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-quiet-no-diff-artifacts',
		workspace_path: quietWorkspace,
		artifacts_path: quietArtifactDir,
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [quietCliPath],
			},
		},
	}, { env: fixtureEnv });
	assert.equal(quietResult.status, 'succeeded');
	assert.deepEqual(quietResult.artifacts.map((artifact) => artifact.name).sort(), ['agent_result', 'patch', 'transcript']);
	assert.equal(quietResult.metadata.missing_declared_artifacts, undefined);
	assert.equal(fs.readFileSync(quietResult.artifacts.find((artifact) => artifact.name === 'patch').path, 'utf8'), '');
	assert.equal(fs.readFileSync(quietResult.artifacts.find((artifact) => artifact.name === 'transcript').path, 'utf8'), '');
	const quietAgentResult = JSON.parse(fs.readFileSync(quietResult.artifacts.find((artifact) => artifact.name === 'agent_result').path, 'utf8'));
	assert.deepEqual(quietAgentResult.artifacts, { patch: false, transcript: false });
	assert.equal(quietAgentResult.status, 'succeeded');

	const blockedArtifactPath = path.join(root, 'artifact-root-is-a-file');
	fs.mkdirSync(blockedArtifactPath);
	const captureFailureCliPath = path.join(root, 'mock-opencode-break-artifact-root.cjs');
	fs.writeFileSync(captureFailureCliPath, `#!/usr/bin/env node
const fs = require('node:fs');
fs.rmSync(${JSON.stringify(blockedArtifactPath)}, { recursive: true, force: true });
fs.writeFileSync(${JSON.stringify(blockedArtifactPath)}, 'not a directory');
process.exit(0);
`);
	const captureFailureResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-artifact-capture-failure',
		workspace_path: quietWorkspace,
		artifacts_path: blockedArtifactPath,
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [captureFailureCliPath],
			},
		},
	}, { env: fixtureEnv });
	assert.equal(captureFailureResult.status, 'provider_error');
	assert.equal(captureFailureResult.failure_code, 'agent_task.opencode_artifact_capture_failed');
	assert.equal(captureFailureResult.metadata.artifact_capture_errors.length, 3);
	assert.equal(captureFailureResult.diagnostics.some((diagnostic) => diagnostic.class === 'opencode.artifact_capture_failed'), true);

	const deniedWorkspace = path.join(root, 'denied-workspace');
	const deniedArtifactDir = path.join(root, 'denied-artifacts');
	fs.mkdirSync(deniedWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: deniedWorkspace, encoding: 'utf8' });
	fs.writeFileSync(path.join(deniedWorkspace, 'README.md'), 'unchanged\n');
	spawnSync('git', ['add', 'README.md'], { cwd: deniedWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '-m', 'initial'], {
		cwd: deniedWorkspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});
	const recoveredCliPath = path.join(root, 'mock-opencode-policy-denied-then-completed.cjs');
	fs.writeFileSync(recoveredCliPath, `#!/usr/bin/env node
process.stdout.write(JSON.stringify({
  type: 'message',
  timestamp: '2026-07-03T15:12:00.000Z',
  parts: [{
    type: 'tool',
    tool: 'bash',
    input: { command: 'cd /tmp && git clone https://example.invalid/private.git' },
    state: { error: 'The user rejected permission to use this specific tool call.' }
  }]
}) + '\\n');
process.stdout.write(JSON.stringify({
  type: 'text',
  part: { type: 'text', text: 'Completed after the denied tool call.', metadata: { openai: { phase: 'final_answer' } } }
}) + '\\n');
process.exit(0);
`);
	const recoveredResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-policy-denied-then-completed',
		workspace_path: deniedWorkspace,
		artifacts_path: deniedArtifactDir,
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [recoveredCliPath],
			},
		},
	}, { env: fixtureEnv });
	assert.equal(recoveredResult.status, 'succeeded');
	assert.equal(recoveredResult.failure_classification, undefined);
	assert.equal(recoveredResult.failure_code, undefined);
	assert.equal(recoveredResult.metadata.denied_tool_call, undefined);
	assert.deepEqual(recoveredResult.artifacts.map((artifact) => artifact.name).sort(), ['agent_result', 'opencode-runtime-stdout', 'patch', 'progress_events', 'transcript']);
	assert.equal(recoveredResult.diagnostics.some((diagnostic) => diagnostic.class === 'opencode.policy_denied'), false);
	assert.deepEqual(recoveredResult.metadata.opencode_progress, { emitted: 1, coalesced_or_dropped: 0, last_type: 'command.failed' });
	const recoveredProgress = fs.readFileSync(recoveredResult.artifacts.find((artifact) => artifact.name === 'progress_events').path, 'utf8');
	assert.match(recoveredProgress, /"type":"command.failed"/);
	assert.equal(recoveredProgress.includes('/tmp'), false);
	const recoveredAgentResult = JSON.parse(fs.readFileSync(recoveredResult.artifacts.find((artifact) => artifact.name === 'agent_result').path, 'utf8'));
	assert.equal(recoveredAgentResult.status, 'succeeded');
	assert.equal(recoveredAgentResult.failure_classification, undefined);

	const deniedCliPath = path.join(root, 'mock-opencode-policy-denied.cjs');
	fs.writeFileSync(deniedCliPath, `#!/usr/bin/env node
process.stderr.write('permission requested: external_directory (${path.join(deniedWorkspace, '*')}); auto-rejecting\\n');
process.stdout.write(JSON.stringify({
  type: 'message',
  timestamp: '2026-07-03T15:12:00.000Z',
  parts: [{
    type: 'tool',
    tool: 'bash',
    input: { command: 'cd /tmp && git clone https://example.invalid/private.git' },
    state: { error: 'The user rejected permission to use this specific tool call.' }
  }]
}) + '\\n');
process.exit(1);
`);
	const deniedResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-policy-denied',
		workspace_path: deniedWorkspace,
		artifacts_path: deniedArtifactDir,
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [deniedCliPath],
			},
		},
	}, { env: fixtureEnv });
	assert.equal(deniedResult.status, 'failed');
	assert.equal(deniedResult.failure_classification, 'policy_denied');
	assert.equal(deniedResult.failure_code, 'agent_task.opencode_policy_denied');
	assert.equal(deniedResult.failure_category, 'task.policy_denied');
	assert.equal(deniedResult.retryable, false);
	assert.equal(deniedResult.metadata.missing_declared_artifacts, undefined);
	assert.deepEqual(deniedResult.artifacts.map((artifact) => artifact.name).sort(), ['agent_result', 'opencode-runtime-stderr', 'opencode-runtime-stdout', 'patch', 'progress_events', 'transcript']);
	assert.deepEqual(deniedResult.metadata.denied_tool_call, {
		tool: 'bash',
		command: 'cd /tmp && git clone https://example.invalid/private.git',
		timestamp: '2026-07-03T15:12:00.000Z',
		permission: 'external_directory',
		path: path.join(deniedWorkspace, '*'),
	});
	assert.equal(deniedResult.diagnostics.some((diagnostic) => diagnostic.class === 'opencode.policy_denied'), true);
	assert.match(deniedResult.diagnostics.find((diagnostic) => diagnostic.class === 'opencode.policy_denied').message, /external_directory/);
	assert.match(fs.readFileSync(deniedResult.artifacts.find((artifact) => artifact.name === 'transcript').path, 'utf8'), /rejected permission/);
	assert.equal(fs.readFileSync(deniedResult.artifacts.find((artifact) => artifact.name === 'patch').path, 'utf8'), '');
	const deniedAgentResult = JSON.parse(fs.readFileSync(deniedResult.artifacts.find((artifact) => artifact.name === 'agent_result').path, 'utf8'));
	assert.equal(deniedAgentResult.status, 'failed');
	assert.equal(deniedAgentResult.failure_classification, 'policy_denied');
	assert.deepEqual(deniedAgentResult.denied_tool_call, deniedResult.metadata.denied_tool_call);

	const deniedZeroCliPath = path.join(root, 'mock-opencode-policy-denied-zero.cjs');
	fs.writeFileSync(deniedZeroCliPath, fs.readFileSync(deniedCliPath, 'utf8').replace('process.exit(1);', 'process.exit(0);'));
	const deniedZeroResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-policy-denied-zero',
		workspace_path: deniedWorkspace,
		artifacts_path: deniedArtifactDir,
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [deniedZeroCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(deniedZeroResult.status, 'failed');
	assert.equal(deniedZeroResult.failure_classification, 'policy_denied');
	assert.equal(deniedZeroResult.retryable, false);

	const inheritedPipeCliPath = path.join(root, 'mock-opencode-inherited-pipe.cjs');
	fs.writeFileSync(inheritedPipeCliPath, `#!/usr/bin/env node
const { spawn } = require('node:child_process');
spawn(process.execPath, ['-e', 'setTimeout(() => {}, 10000)'], { detached: true, stdio: ['ignore', 'inherit', 'inherit'] }).unref();
process.stdout.write('parent finished');
process.exit(0);
`);
	const inheritedPipeResult = await Promise.race([
		executeOpenCodeAgentTask({
			...request,
			task_id: 'opencode-inherited-pipe-exit',
			executor: {
				...request.executor,
				config: {
					...request.executor.config,
					command_args: [inheritedPipeCliPath],
				},
			},
		}, { env: fixtureEnv }),
		new Promise((_, reject) => setTimeout(() => reject(new Error('OpenCode executor hung on inherited stdio pipes')), 2000)),
	]);
	assert.equal(inheritedPipeResult.status, 'succeeded');
	assert.match(inheritedPipeResult.diagnostics[0].message, /status 0/);

	const quotaVariants = [
		'AI_APICallError: Weekly Limit Exhausted. Your limit will reset at 2026-07-20T00:00:00Z.',
		'AI_APICallError: Monthly Limit Exhausted. Your limit will reset at 2026-08-01T00:00:00Z.',
		'AI_APICallError: Provider quota exhausted.',
		'AI_APICallError: Provider quota exceeded.',
		'AI_APICallError: Rate limit exceeded.',
		'AI_APICallError: Usage limit exhausted.',
		'AI_APICallError: Usage limit reached for 5 hours. Your limit will reset later.',
	];
	for (const [index, quotaError] of quotaVariants.entries()) {
		const quotaInvocationPath = path.join(root, `opencode-quota-${index}-invocations`);
		const quotaTerminationPath = path.join(root, `opencode-quota-${index}-terminated`);
		const quotaCliPath = path.join(root, `mock-opencode-quota-${index}.cjs`);
		fs.writeFileSync(quotaCliPath, `#!/usr/bin/env node
const fs = require('node:fs');
fs.appendFileSync(${JSON.stringify(quotaInvocationPath)}, 'started\\n');
process.on('SIGTERM', () => {
  fs.writeFileSync(${JSON.stringify(quotaTerminationPath)}, 'SIGTERM');
  process.exit(0);
});
const event = JSON.stringify({ type: 'error', error: ${JSON.stringify(quotaError)}, token: process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN }) + '\\n';
process.stderr.write(event.slice(0, 24));
setTimeout(() => process.stderr.write(event.slice(24)), 5);
setInterval(() => process.stderr.write('OpenCode retrying after provider backoff\\n'), 25);
`);
		const quotaResult = await Promise.race([
			executeOpenCodeAgentTask({
				...request,
				task_id: `opencode-quota-fail-fast-${index}`,
				executor: {
					...request.executor,
					config: { ...request.executor.config, command_args: [quotaCliPath] },
				},
			}, { env: fixtureEnv }),
			new Promise((_, reject) => setTimeout(() => reject(new Error('OpenCode executor did not fail fast on provider quota exhaustion')), 2000)),
		]);
		assert.equal(quotaResult.status, 'provider_error');
		assert.equal(quotaResult.failure_code, 'agent_task.opencode_usage_limit');
		assert.equal(quotaResult.failure_classification, 'provider_quota');
		assert.equal(quotaResult.failure_category, 'provider.quota');
		assert.equal(quotaResult.retryable, false);
		assert.deepEqual(quotaResult.diagnostics, [{
			class: 'opencode.provider_quota',
			message: 'OpenCode reported a terminal provider quota limit. Wait for the provider limit to reset or select an available provider/model, then retry.',
			data: {},
		}]);
		assert.equal(fs.readFileSync(quotaInvocationPath, 'utf8'), 'started\n');
		assert.equal(fs.readFileSync(quotaTerminationPath, 'utf8'), 'SIGTERM');
		assert.equal(JSON.stringify(quotaResult).includes('access-token-must-not-leak'), false);
		for (const artifact of quotaResult.artifacts) {
			assert.equal(fs.readFileSync(artifact.path, 'utf8').includes('access-token-must-not-leak'), false);
		}
	}

	const transientQuotaCliPath = path.join(root, 'mock-opencode-transient-rate-limit.cjs');
	fs.writeFileSync(transientQuotaCliPath, `#!/usr/bin/env node
process.stderr.write(JSON.stringify({ type: 'error', error: 'AI_APICallError: Rate limit reached; retrying in 1 second.' }) + '\\n');
process.exit(0);
`);
	const transientQuotaResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-transient-rate-limit',
		executor: {
			...request.executor,
			config: { ...request.executor.config, command_args: [transientQuotaCliPath] },
		},
	}, { env: fixtureEnv });
	assert.equal(transientQuotaResult.status, 'succeeded');

	const implicitArtifactResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-implicit-artifact-dir',
		workspace_path: quietWorkspace,
		artifacts_path: undefined,
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [quietCliPath],
			},
		},
	}, { env: fixtureEnv });
	assert.equal(implicitArtifactResult.status, 'succeeded');
	assert.deepEqual(implicitArtifactResult.artifacts.map((artifact) => artifact.name).sort(), ['agent_result', 'patch', 'transcript']);
	assert.equal(implicitArtifactResult.metadata.missing_declared_artifacts, undefined);
	assert.equal(
		implicitArtifactResult.artifacts.every((artifact) => artifact.path.startsWith(path.join(quietWorkspace, '.homeboy', 'opencode'))),
		true
	);

	const workspaceRootResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-workspace-root-artifact-dir',
		artifacts_path: undefined,
		workspace: {
			mode: 'existing',
			root: quietWorkspace,
		},
		expected_artifacts: ['patch', 'transcript', 'agent_result'],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [quietCliPath],
				workspace_root: quietWorkspace,
			},
		},
	}, { env: fixtureEnv });
	assert.equal(workspaceRootResult.status, 'succeeded');
	assert.deepEqual(workspaceRootResult.artifacts.map((artifact) => artifact.name).sort(), ['agent_result', 'patch', 'transcript']);
	assert.equal(workspaceRootResult.metadata.missing_declared_artifacts, undefined);
	assert.equal(
		workspaceRootResult.artifacts.every((artifact) => artifact.path.startsWith(path.join(quietWorkspace, '.homeboy', 'opencode'))),
		true
	);

	// #8829: a review-only cook that only inspects an existing candidate can be
	// dispatched without a workspace path that resolves to an absolute directory.
	// The read-only inspection tools (read/glob/grep) must still be permitted
	// within the workspace — otherwise OpenCode falls back to its default `ask`,
	// is denied non-interactively, and the cook fails with `opencode.policy_denied`
	// before it can inspect the candidate. Reads outside the workspace stay denied.
	const reviewCapturePath = path.join(root, 'opencode-review-config.json');
	const reviewCliPath = path.join(root, 'mock-opencode-review.cjs');
	fs.writeFileSync(reviewCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
const fs = require('node:fs');
const config = JSON.parse(process.env.OPENCODE_CONFIG_CONTENT || '{}');
const prompt = process.argv.at(-1);
const declarations = JSON.parse(prompt.match(/Output declarations: (\\[.*\\])\\.$/s)?.[1] || 'null');
assert.deepEqual(declarations, [{
 name: 'review_form', required: true, schema: 'homeboy/agent-task-review-form/v1', json_schema: {
  type: 'object', required: ['summary', 'what_changed', 'compatibility', 'used_for'],
  properties: {
   summary: { type: 'string' },
   what_changed: { type: 'array', items: { type: 'string' } },
   compatibility: { type: 'string' },
   used_for: { type: 'string' }
  }
 }
}]);
fs.writeFileSync(${JSON.stringify(reviewCapturePath)}, JSON.stringify(config));
process.stdout.write(JSON.stringify({
 type: 'text',
 part: { type: 'text', text: JSON.stringify({ outputs: { review_form: {
   summary: 'Reviewed candidate; no changes required.',
   what_changed: [],
   compatibility: 'No compatibility impact.',
   used_for: 'Pull request review.'
 } } }), metadata: { openai: { phase: 'final_answer' } } }
}));
`);
	// A review-only cook whose workspace is referenced by a *relative* cwd (the
	// #8829 trigger): it names a real, existing directory but does not resolve to
	// an absolute path, so no concrete workspace read patterns are generated. The
	// base permission denies only `*.env` reads with no catch-all allow rule.
	const reviewWorkspace = path.join(root, 'review-workspace');
	fs.mkdirSync(reviewWorkspace, { recursive: true });
	spawnSync('git', ['init'], { cwd: reviewWorkspace, encoding: 'utf8' });
	spawnSync('git', ['commit', '--allow-empty', '-m', 'candidate'], {
		cwd: reviewWorkspace,
		encoding: 'utf8',
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Homeboy Test',
			GIT_AUTHOR_EMAIL: 'homeboy@example.test',
			GIT_COMMITTER_NAME: 'Homeboy Test',
			GIT_COMMITTER_EMAIL: 'homeboy@example.test',
		},
	});
	const relativeReviewCwd = path.relative(process.cwd(), reviewWorkspace);
	assert.equal(path.isAbsolute(relativeReviewCwd), false);
	const reviewResult = await executeOpenCodeAgentTask({
		...request,
		task_id: 'opencode-review-only',
		instructions: 'Review the existing candidate and preserve it when correct.',
		policy: { write: 'none' },
		output_declarations: [{
			name: 'review_form',
			required: true,
			schema: 'homeboy/agent-task-review-form/v1',
			structural_schema: {
				type: 'object',
				required: ['summary', 'what_changed', 'compatibility', 'used_for'],
				properties: {
					summary: { type: 'string' },
					what_changed: { type: 'array', items: { type: 'string' } },
					compatibility: { type: 'string' },
					used_for: { type: 'string' },
				},
			},
		}],
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [reviewCliPath],
				cwd: relativeReviewCwd,
				runtime_env: {
					OPENCODE_CONFIG_CONTENT: JSON.stringify({ permission: { read: { '*.env': 'deny' } } }),
				},
			},
		},
	}, { env: fixtureEnv });

	assert.equal(reviewResult.status, 'succeeded', JSON.stringify(reviewResult.diagnostics));
	assert.deepEqual(reviewResult.outputs.review_form, {
			summary: 'Reviewed candidate; no changes required.',
			what_changed: [],
			compatibility: 'No compatibility impact.',
			used_for: 'Pull request review.',
	});
	assert.equal(reviewResult.outputs.opencode_run_result.intentional_no_change.schema, 'homeboy/intentional-no-change/v1');
	assert.equal(reviewResult.outputs.opencode_run_result.intentional_no_change.verdict, 'no_change');
	assert.match(reviewResult.outputs.opencode_run_result.intentional_no_change.inspected_revision, /^[0-9a-f]{40}$/);
	const reviewConfig = JSON.parse(fs.readFileSync(reviewCapturePath, 'utf8'));
	// Read-only inspection is permitted within the workspace...
	assert.equal(reviewConfig.permission.read['*'], 'allow');
	assert.equal(reviewConfig.agent.build.permission.read['*'], 'allow');
	assert.equal(nativeToolAction(reviewConfig, 'build', 'glob', 'src/**/*.js'), 'allow');
	assert.equal(nativeToolAction(reviewConfig, 'build', 'grep', 'TODO'), 'allow');
	assert.equal(nativeToolAction(reviewConfig, 'build', 'edit', 'src/index.js'), 'deny');
	assert.equal(nativeToolAction(reviewConfig, 'build', 'bash', 'git status --short'), 'deny');
	assert.deepEqual(reviewConfig.permission.edit, { '*': 'deny' });
	assert.deepEqual(reviewConfig.permission.bash, { '*': 'deny' });
	// ...while reads outside the workspace and the pre-existing secret deny remain denied.
	assert.equal(reviewConfig.permission.read['..'], 'deny');
	assert.equal(reviewConfig.permission.read['../*'], 'deny');
	assert.equal(reviewConfig.permission.read['*.env'], 'deny');

	const structuredOutputCliPath = path.join(root, 'mock-opencode-structured-outputs.cjs');
	fs.writeFileSync(structuredOutputCliPath, `#!/usr/bin/env node
const mode = process.env.OUTPUT_MODE;
const prompt = process.argv.at(-1);
const declarations = JSON.parse(prompt.match(/Output declarations: (\\[.*\\])\\.$/s)?.[1] || 'null');
if (declarations?.[0]?.name !== 'release_notes' || declarations[0].required !== true || declarations?.[1]?.name !== 'verification' || declarations[1].required !== false) {
  throw new Error('OpenCode did not receive generic output declarations.');
}
const text = mode === 'valid'
	? ${JSON.stringify(`\`\`\`json\n${JSON.stringify({ outputs: { release_notes: ['Added generic output handling.'], verification: { passed: true } } }, null, 2)}\n\`\`\``)}
	: mode === 'malformed'
		? ${JSON.stringify('```json\n{"outputs":{"release_notes":{"unexpected":[]},"verification":false}}\n```')}
	: mode === 'oversized'
			? JSON.stringify({ outputs: { release_notes: 'x'.repeat(70 * 1024) } })
			: mode === 'optional-absent'
				? ${JSON.stringify('```json\n{"outputs":{"release_notes":["Optional output omitted"]}}\n```')}
			: mode === 'legacy'
				? ${JSON.stringify('```json\n{"release_notes":["Legacy declaration mapping"],"ignored":"not declared"}\n```')}
				: 'Completed without structured output.';
process.stdout.write(JSON.stringify({
	type: 'text',
	part: { type: 'text', text, metadata: { openai: { phase: 'final_answer' } } },
}) + '\\n');
`);
	const structuredOutputRequest = {
		...request,
		task_id: 'opencode-structured-outputs',
		inputs: {
			required_outputs: [
				{ name: 'release_notes', required: true, json_schema: { type: 'array', items: { type: 'string' } } },
				{ name: 'verification', required: false, json_schema: { type: 'object' } },
			],
		},
		workspace: { root: reviewWorkspace },
		executor: {
			...request.executor,
			config: {
				...request.executor.config,
				command_args: [structuredOutputCliPath],
				cwd: reviewWorkspace,
				runtime_env: { OUTPUT_MODE: 'valid' },
			},
		},
	};
	const structuredOutputResult = await executeOpenCodeAgentTask(structuredOutputRequest, { env: fixtureEnv });
	assert.equal(structuredOutputResult.status, 'succeeded', JSON.stringify(structuredOutputResult.diagnostics));
	assert.deepEqual(structuredOutputResult.outputs.release_notes, ['Added generic output handling.']);
	assert.deepEqual(structuredOutputResult.outputs.verification, { passed: true });
	assert.equal(structuredOutputResult.outputs.opencode_run_result.intentional_no_change.schema, 'homeboy/intentional-no-change/v1');
	assert.equal(structuredOutputResult.metadata.opencode_session.status, 'not_discovered');
	const structuredAgentResult = structuredOutputResult.artifacts.find((artifact) => artifact.name === 'agent_result');
	const structuredAgentResultPayload = JSON.parse(fs.readFileSync(structuredAgentResult.path, 'utf8'));
	assert.equal(structuredAgentResultPayload.status, 'succeeded');
	assert.deepEqual(structuredAgentResultPayload.outputs, structuredOutputResult.outputs);

	for (const [mode, outputs, diagnosticClass] of [
		['missing', {}, 'opencode.required_outputs_missing'],
		['malformed', { release_notes: { unexpected: [] }, verification: false }, undefined],
		['oversized', {}, 'opencode.declared_outputs_oversized'],
	]) {
		const incompleteOutputResult = await executeOpenCodeAgentTask({
			...structuredOutputRequest,
			task_id: `opencode-structured-outputs-${mode}`,
			executor: {
				...structuredOutputRequest.executor,
				config: {
					...structuredOutputRequest.executor.config,
					runtime_env: { OUTPUT_MODE: mode },
				},
			},
		}, { env: fixtureEnv });
		assert.equal(incompleteOutputResult.status, mode === 'malformed' ? 'succeeded' : 'failed');
		assert.deepEqual(Object.fromEntries(
			Object.entries(incompleteOutputResult.outputs).filter(([name]) => name !== 'opencode_run_result')
		), outputs);
		assert.equal(incompleteOutputResult.diagnostics.some((diagnostic) => diagnostic.class === diagnosticClass), diagnosticClass !== undefined);
	}

	const optionalAbsentResult = await executeOpenCodeAgentTask({
		...structuredOutputRequest,
		task_id: 'opencode-structured-outputs-optional-absent',
		executor: {
			...structuredOutputRequest.executor,
			config: { ...structuredOutputRequest.executor.config, runtime_env: { OUTPUT_MODE: 'optional-absent' } },
		},
	}, { env: fixtureEnv });
	assert.equal(optionalAbsentResult.status, 'succeeded');
	assert.deepEqual(optionalAbsentResult.outputs.release_notes, ['Optional output omitted']);
	assert.equal(optionalAbsentResult.diagnostics.some((diagnostic) => diagnostic.class === 'opencode.required_outputs_missing'), false);

	const legacyOutputResult = await executeOpenCodeAgentTask({
		...structuredOutputRequest,
		task_id: 'opencode-structured-outputs-legacy',
		executor: {
			...structuredOutputRequest.executor,
			config: { ...structuredOutputRequest.executor.config, runtime_env: { OUTPUT_MODE: 'legacy' } },
		},
	}, { env: fixtureEnv });
	assert.deepEqual(legacyOutputResult.outputs.release_notes, ['Legacy declaration mapping']);
	assert.equal(legacyOutputResult.outputs.ignored, undefined);
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('OpenCode agent task executor boundary passed\n');
})().catch((error) => {
	process.stderr.write(`${error.stack || error.message}\n`);
	process.exit(1);
});
