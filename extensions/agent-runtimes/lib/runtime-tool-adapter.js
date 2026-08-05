'use strict';

const RESOLVED_RUNTIME_TOOL_SCHEMA = 'homeboy/resolved-agent-task-runtime-tool/v1';
const RAW_RUNTIME_TOOLS_ENV = 'HOMEBOY_AGENT_TASK_RUNTIME_TOOLS_JSON';
const RESOLVED_RUNTIME_TOOLS_ENV = 'HOMEBOY_AGENT_TASK_RESOLVED_RUNTIME_TOOLS_JSON';

function resolvedRuntimeTools(request = {}, env = process.env) {
	if (hasRuntimeToolDeclarations(request) || hasRawRuntimeToolDeclarationsEnv(env)) {
		throw new Error('Runtime tool declarations must be resolved by Homeboy before provider dispatch.');
	}
	if (Array.isArray(request.resolved_runtime_tools)) {
		return request.resolved_runtime_tools.map(validateRuntimeTool);
	}
	const projected = resolvedRuntimeToolsFromEnv(env);
	if (projected) return projected.map(validateRuntimeTool);
	return [];
}

function validateRuntimeTool(tool) {
	const argv = tool?.argv;
	if (!tool || tool.schema !== RESOLVED_RUNTIME_TOOL_SCHEMA || typeof tool.id !== 'string' || !validId(tool.id)
		|| tool.transport !== 'stdio' || !Array.isArray(argv) || argv.length === 0
		|| argv.some((part) => typeof part !== 'string' || part.trim() === '')
		|| typeof tool.executable !== 'string' || tool.executable !== argv[0]
		|| !readyRuntimeToolEvidence(tool.readiness, tool.capabilities)
		|| (tool.lifecycle !== undefined && tool.lifecycle !== 'runtime_owned')) {
		throw new Error(`Invalid or unready runtime tool projection: ${tool?.id || 'unknown'}.`);
	}
	const values = objectValue(tool.env);
	const envNames = arrayValue(tool.secret_env_names || tool.secret_env);
	if (Object.keys(values).some((name) => !validEnvName(name)) || envNames.some((name) => !validEnvName(name))) {
		throw new Error(`Runtime tool '${tool.id}' has an invalid environment name.`);
	}
	return {
		schema: tool.schema,
		id: tool.id,
		argv: [...argv],
		executable: tool.executable,
		env: values,
		secret_env_names: [...new Set(envNames)],
		capabilities: arrayValue(tool.capabilities),
		readiness: tool.readiness,
	};
}

function hasRuntimeToolDeclarations(request) {
	return Array.isArray(request?.runtime_tools) && request.runtime_tools.length > 0;
}

function hasRawRuntimeToolDeclarationsEnv(env) {
	try {
		const declarations = JSON.parse(env?.[RAW_RUNTIME_TOOLS_ENV] || '[]');
		return Array.isArray(declarations) && declarations.length > 0;
	} catch {
		return Boolean(env?.[RAW_RUNTIME_TOOLS_ENV]);
	}
}

function resolvedRuntimeToolsFromEnv(env) {
	if (!env?.[RESOLVED_RUNTIME_TOOLS_ENV]) return null;
	try {
		const tools = JSON.parse(env[RESOLVED_RUNTIME_TOOLS_ENV]);
		if (!Array.isArray(tools)) throw new Error('not an array');
		return tools;
	} catch {
		throw new Error('Invalid resolved runtime tool projection from Homeboy.');
	}
}

function readyRuntimeToolEvidence(readiness, capabilities) {
	if (!readiness || typeof readiness !== 'object' || Array.isArray(readiness) || readiness.status !== 'ready') {
		return false;
	}
	if (!Array.isArray(capabilities) || capabilities.length === 0) {
		return true;
	}
	const evidence = readiness.evidence;
	return evidence && typeof evidence === 'object' && !Array.isArray(evidence)
		&& evidence.success === true
		&& ['version_command', 'protocol', 'declared_probe'].includes(evidence.kind);
}

function applyOpenCodeRuntimeTools(content = {}, request = {}, env = process.env) {
	const tools = resolvedRuntimeTools(request, env);
	if (tools.length === 0) {
		return content;
	}
	const mcp = objectValue(content.mcp);
	for (const tool of tools) {
		mcp[tool.id] = {
			type: 'local',
			command: tool.argv[0],
			args: tool.argv.slice(1),
			environment: runtimeToolEnvironment(tool, env),
		};
	}
	return { ...content, mcp };
}

function codexRuntimeToolConfigArgs(request = {}, env = process.env) {
	return resolvedRuntimeTools(request, env).flatMap((tool) => {
		const key = `mcp_servers.${tool.id}`;
		return [
			'--config', `${key}.command=${tomlString(tool.argv[0])}`,
			'--config', `${key}.args=${tomlValue(tool.argv.slice(1))}`,
			'--config', `${key}.env=${tomlValue(runtimeToolEnvironment(tool, env))}`,
		];
	});
}

function adapterRuntimeToolRequest(request = {}, env = process.env) {
	const tools = resolvedRuntimeTools(request, env);
	return tools.length === 0 ? request : { ...request, resolved_runtime_tools: tools.map((tool) => ({
		...tool,
		env: runtimeToolEnvironment(tool, env),
		lifecycle: 'runtime_owned',
	})) };
}

function runtimeToolSecretEnvNames(request = {}) {
	return resolvedRuntimeTools(request).flatMap((tool) => tool.secret_env_names);
}

function runtimeToolEnvironment(tool, env) {
	return Object.fromEntries([
		...Object.entries(tool.env),
		...tool.secret_env_names.filter((name) => env?.[name] !== undefined).map((name) => [name, env[name]]),
	]);
}

function tomlValue(value) {
	if (Array.isArray(value)) return `[${value.map(tomlString).join(', ')}]`;
	return `{ ${Object.entries(value).map(([key, entry]) => `${key} = ${tomlString(entry)}`).join(', ')} }`;
}

function tomlString(value) {
	return JSON.stringify(String(value));
}

function validId(value) {
	return /^[A-Za-z0-9._-]+$/.test(value);
}

function validEnvName(value) {
	return typeof value === 'string' && /^[A-Za-z_][A-Za-z0-9_]*$/.test(value);
}

function arrayValue(value) {
	return Array.isArray(value) ? value : [];
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

module.exports = {
	RESOLVED_RUNTIME_TOOL_SCHEMA,
	RAW_RUNTIME_TOOLS_ENV,
	RESOLVED_RUNTIME_TOOLS_ENV,
	adapterRuntimeToolRequest,
	applyOpenCodeRuntimeTools,
	codexRuntimeToolConfigArgs,
	resolvedRuntimeTools,
	runtimeToolSecretEnvNames,
};
