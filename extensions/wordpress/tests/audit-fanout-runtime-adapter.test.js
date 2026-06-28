'use strict';

const assert = require('node:assert/strict');
const {
  auditFanoutRuntimeInvocation,
} = require('../lib/audit-fanout-runtime-adapter');

assert.throws(
  () => auditFanoutRuntimeInvocation({ registry: {} }),
  /requires explicit runtime=wp-codebox or an explicit command/
);

const explicit = auditFanoutRuntimeInvocation({ command: 'custom-runtime', args: ['run'], registry: {} });
assert.equal(explicit.runtime, null);
assert.equal(explicit.command, 'custom-runtime');
assert.deepEqual(explicit.args, ['run']);

const runtimeInvocation = auditFanoutRuntimeInvocation({
  runtime: 'wp-codebox',
  registry: {
    'wp-codebox': {
      schema: 'homeboy/agent-runtime-manifest/v1',
      id: 'wp-codebox',
      agent_task_executors: [{ id: 'wp-codebox.fixture', backend: 'wp-codebox', invocation: { argv: ['node', '{{runtime_path}}/fixture.cjs'] } }],
    },
  },
  repoRoot: '/repo',
});
assert.equal(runtimeInvocation.command, process.execPath);
assert.deepEqual(runtimeInvocation.args, ['/repo/agent-runtimes/wp-codebox/fixture.cjs']);

process.stdout.write('Audit fanout runtime adapter checks passed\n');
