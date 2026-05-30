#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-invocation-runtime.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

HELPER_UNDER_TEST="$SCRIPT_DIR/invocation-runtime.mjs" \
TMP_ROOT="$TMP_DIR" \
node --input-type=module - <<'EOF'
import { access } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const { resolveHomeboyInvocationRuntime } = await import(process.env.HELPER_UNDER_TEST);
const tmpRoot = process.env.TMP_ROOT;

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function assertDir(path, message) {
  await access(path).catch(() => {
    throw new Error(message);
  });
}

function assertThrows(fn, expected, message) {
  try {
    fn();
  } catch (error) {
    if (!String(error.message).includes(expected)) {
      throw new Error(`${message}: expected "${expected}", got "${error.message}"`);
    }
    return;
  }
  throw new Error(`${message}: expected throw`);
}

const noIsolation = resolveHomeboyInvocationRuntime({ namespace: 'Smoke Runtime', env: { PATH: '/bin' } });
assert(noIsolation.isolated === false, 'no-isolation runtime should not be isolated');
assert(noIsolation.invocationId === null, 'no-isolation invocation id should be null');
assert(noIsolation.dirs.state === null, 'no-isolation state dir should be null');
assert(noIsolation.portRange === null, 'no-isolation port range should be null');
assert(noIsolation.env.PATH === '/bin', 'no-isolation env should preserve source env');
assert(noIsolation.assertPort('1234') === 1234, 'no-isolation assertPort should normalize without range');

const isolatedEnv = {
  PATH: '/bin',
  HOMEBOY_INVOCATION_ID: 'inv-123',
  HOMEBOY_INVOCATION_STATE_DIR: join(tmpRoot, 'state'),
  HOMEBOY_INVOCATION_ARTIFACT_DIR: join(tmpRoot, 'artifacts'),
  HOMEBOY_INVOCATION_TMP_DIR: join(tmpRoot, 'tmp'),
};
const isolated = resolveHomeboyInvocationRuntime({ namespace: 'Smoke Runtime', env: isolatedEnv });
const scoped = 'Smoke-Runtime';
assert(isolated.isolated === true, 'isolated runtime should be isolated');
assert(isolated.namespace === scoped, 'namespace should be sanitized');
assert(isolated.invocationId === 'inv-123', 'invocation id should be preserved');
assert(isolated.dirs.state === resolve(tmpRoot, 'state', scoped), 'state dir should be namespace-scoped');
assert(isolated.dirs.artifact === resolve(tmpRoot, 'artifacts', scoped), 'artifact dir should be namespace-scoped');
assert(isolated.dirs.tmp === resolve(tmpRoot, 'tmp', scoped), 'tmp dir should be namespace-scoped');
assert(isolated.dirs.home === join(isolated.dirs.state, 'home'), 'home dir should derive from scoped state');
await isolated.prepareDirs();
await assertDir(isolated.dirs.state, 'state dir was not created');
await assertDir(isolated.dirs.artifact, 'artifact dir was not created');
await assertDir(isolated.dirs.tmp, 'tmp dir was not created');
await assertDir(isolated.dirs.home, 'home dir was not created');

const validRange = resolveHomeboyInvocationRuntime({
  namespace: 'ports',
  env: {
    HOMEBOY_INVOCATION_PORT_BASE: '4100',
    HOMEBOY_INVOCATION_PORT_MAX: '4199',
  },
});
assert(validRange.portRange.base === 4100, 'valid port base should parse');
assert(validRange.portRange.max === 4199, 'valid port max should parse');
assert(validRange.assertPort(4100) === 4100, 'port base should validate');
assert(validRange.assertPort('4199') === 4199, 'port max should validate');
assertThrows(() => validRange.assertPort(4200), 'outside Homeboy invocation range', 'out-of-range port should fail');

assertThrows(
  () => resolveHomeboyInvocationRuntime({ env: { HOMEBOY_INVOCATION_PORT_BASE: '4200' } }),
  'must be set together',
  'missing max bound should fail'
);
assertThrows(
  () => resolveHomeboyInvocationRuntime({ env: { HOMEBOY_INVOCATION_PORT_MAX: '4200' } }),
  'must be set together',
  'missing base bound should fail'
);
assertThrows(
  () => resolveHomeboyInvocationRuntime({ env: { HOMEBOY_INVOCATION_PORT_BASE: 'abc', HOMEBOY_INVOCATION_PORT_MAX: '4200' } }),
  'must be an integer port',
  'malformed port bound should fail'
);
assertThrows(
  () => resolveHomeboyInvocationRuntime({ env: { HOMEBOY_INVOCATION_PORT_BASE: '4300', HOMEBOY_INVOCATION_PORT_MAX: '4200' } }),
  'must be less than or equal to',
  'inverted port range should fail'
);

assert(isolated.env.HOMEBOY_INVOCATION_STATE_DIR === isolated.dirs.state, 'env should export scoped state dir');
assert(isolated.env.HOMEBOY_INVOCATION_ARTIFACT_DIR === isolated.dirs.artifact, 'env should export scoped artifact dir');
assert(isolated.env.HOMEBOY_INVOCATION_TMP_DIR === isolated.dirs.tmp, 'env should export scoped tmp dir');
assert(isolated.env.TMPDIR === isolated.dirs.tmp, 'env should export TMPDIR');
assert(isolated.env.HOME === isolated.dirs.home, 'env should export isolated HOME');
assert(isolated.env.XDG_CONFIG_HOME === isolated.dirs.config, 'env should export XDG config dir');
assert(isolated.env.XDG_CACHE_HOME === isolated.dirs.cache, 'env should export XDG cache dir');
assert(isolated.env.XDG_DATA_HOME === isolated.dirs.data, 'env should export XDG data dir');
assert(isolated.env.XDG_STATE_HOME === isolated.dirs.state, 'env should export XDG state dir');
assert(isolated.childEnv({ CI: '1' }).CI === '1', 'childEnv should merge extra env');
assert(isolated.childEnv({ HOME: '/override' }).HOME === '/override', 'childEnv extra env should override defaults');
EOF

if ! grep -q 'HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER' "$SCRIPT_DIR/../bench/bench-runner.sh"; then
    echo "bench-runner.sh does not export HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER" >&2
    exit 1
fi

if ! grep -q 'HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER' "$SCRIPT_DIR/../trace/trace-runner.sh"; then
    echo "trace-runner.sh does not export HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER" >&2
    exit 1
fi

echo "Node.js invocation runtime smoke passed."
