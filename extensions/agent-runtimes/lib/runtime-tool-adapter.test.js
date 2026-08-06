'use strict';

const assert = require('node:assert/strict');
const {
	adapterRuntimeToolRequest,
	applyOpenCodeRuntimeTools,
	codexRuntimeToolConfigArgs,
	RESOLVED_RUNTIME_TOOL_SCHEMA,
	RAW_RUNTIME_TOOLS_ENV,
	RESOLVED_RUNTIME_TOOLS_ENV,
	resolvedRuntimeTools,
} = require('./runtime-tool-adapter');

const request = {
	resolved_runtime_tools: [{
		schema: RESOLVED_RUNTIME_TOOL_SCHEMA,
		id: 'fixture.mcp',
		transport: 'stdio',
		argv: ['/fixture/mcp', '--isolated'],
		executable: '/fixture/mcp',
		env: { FIXTURE_MODE: 'isolated' },
		secret_env_names: ['FIXTURE_TOKEN'],
		capabilities: ['fixture'],
		readiness: { status: 'ready', evidence: { kind: 'version_command', success: true } },
		lifecycle: 'runtime_owned',
	}, {
		schema: RESOLVED_RUNTIME_TOOL_SCHEMA,
		id: 'fixture.second',
		transport: 'stdio',
		argv: ['/fixture/second'],
		executable: '/fixture/second',
		env: {},
		secret_env_names: [],
		capabilities: ['second-fixture'],
		readiness: { status: 'ready', evidence: { kind: 'declared_probe', success: true } },
		lifecycle: 'runtime_owned',
	}],
};
const env = { FIXTURE_TOKEN: 'private-token' };

assert.deepEqual(resolvedRuntimeTools(request, env)[0].argv, ['/fixture/mcp', '--isolated']);
assert.deepEqual(resolvedRuntimeTools({}, { [RESOLVED_RUNTIME_TOOLS_ENV]: JSON.stringify(request.resolved_runtime_tools) })[0].argv, ['/fixture/mcp', '--isolated']);
const openCode = applyOpenCodeRuntimeTools({}, request, env);
assert.deepEqual(openCode.mcp['fixture.mcp'], {
	type: 'local', command: '/fixture/mcp', args: ['--isolated'], environment: { FIXTURE_MODE: 'isolated', FIXTURE_TOKEN: 'private-token' },
});
assert.deepEqual(openCode.mcp['fixture.second'], {
	type: 'local', command: '/fixture/second', args: [], environment: {},
});
const codex = codexRuntimeToolConfigArgs(request, env);
assert.equal(codex.includes('mcp_servers.fixture.mcp.command="/fixture/mcp"'), true);
assert.equal(codex.includes('mcp_servers.fixture.mcp.args=["--isolated"]'), true);
assert.equal(codex.some((value) => value.includes('private-token')), true);
assert.equal(codex.includes('mcp_servers.fixture.second.command="/fixture/second"'), true);
const adapter = adapterRuntimeToolRequest(request, env);
assert.deepEqual(adapter.resolved_runtime_tools[0].argv, ['/fixture/mcp', '--isolated']);
assert.equal(adapter.resolved_runtime_tools[0].env.FIXTURE_TOKEN, 'private-token');
assert.deepEqual(adapter.resolved_runtime_tools[1].argv, ['/fixture/second']);
assert.throws(() => resolvedRuntimeTools({ resolved_runtime_tools: [{ ...request.resolved_runtime_tools[0], readiness: 'missing' }] }), /unready/);
assert.throws(() => resolvedRuntimeTools({ resolved_runtime_tools: [{ ...request.resolved_runtime_tools[0], lifecycle: 'caller_owned' }] }), /unready/);
assert.throws(() => resolvedRuntimeTools({ runtime_tools: [{ id: 'raw.echo', command: ['/bin/echo', 'unsafe'] }] }), /resolved by Homeboy/);
assert.throws(() => resolvedRuntimeTools({ resolved_runtime_tools: [], runtime_tools: [{ id: 'raw.echo', command: ['/bin/echo', 'unsafe'] }] }), /resolved by Homeboy/);
assert.throws(() => resolvedRuntimeTools({ resolved_runtime_tools: [] }, { [RAW_RUNTIME_TOOLS_ENV]: JSON.stringify([{ id: 'raw.echo', command: ['/bin/echo', 'unsafe'] }]) }), /resolved by Homeboy/);
assert.throws(() => resolvedRuntimeTools({ resolved_runtime_tools: [{ ...request.resolved_runtime_tools[0], readiness: { status: 'ready' } }] }), /unready/);

process.stdout.write('Runtime tool adapter boundary passed\n');
