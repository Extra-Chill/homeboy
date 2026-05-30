#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-workload-utils.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
mkdir -p "$TMP_DIR/component-under-test"

HOMEBOY_COMPONENT_PATH="$TMP_DIR/component-under-test" \
HOMEBOY_SETTINGS_JSON='{"alpha":"json-value","number":42,"namespace":"json namespace","ignored_null":null,"enabled":true,"list":["json","items"],"object":{"source":"json"},"json_text":"{\"source\":\"json-string\"}","relative_path":"reports/out.json"}' \
HOMEBOY_SETTINGS_BETA='env-value' \
HOMEBOY_SETTINGS_ENV_INT='17' \
HOMEBOY_SETTINGS_ENV_BOOL='off' \
HOMEBOY_SETTINGS_ENV_LIST='one, two,,three' \
HOMEBOY_SETTINGS_ENV_JSON='{"source":"env"}' \
WORKLOAD_UTILS_UNDER_TEST="$SCRIPT_DIR/lib/workload-utils.mjs" \
node --input-type=module - <<'EOF'
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const utils = await import(process.env.WORKLOAD_UTILS_UNDER_TEST);

if (utils.setting('alpha') !== 'json-value') throw new Error('JSON setting was not resolved');
if (utils.setting('number') !== '42') throw new Error('numeric JSON setting was not coerced to string');
if (utils.setting('beta') !== 'env-value') throw new Error('env setting fallback was not resolved');
if (utils.setting('ignored_null', 'fallback') !== 'fallback') throw new Error('null JSON setting should use fallback');
if (utils.setting('missing', 'fallback') !== 'fallback') throw new Error('missing setting fallback failed');

if (utils.settingInt('number') !== 42) throw new Error('JSON integer setting was not parsed');
if (utils.settingInt('env_int') !== 17) throw new Error('env integer setting was not parsed');
if (utils.settingInt('env_int', 0, { max: 10 }) !== 0) throw new Error('integer max bound was not enforced');
if (utils.settingInt('alpha', 9) !== 9) throw new Error('invalid integer did not use fallback');

if (utils.settingBool('enabled') !== true) throw new Error('JSON boolean setting was not parsed');
if (utils.settingBool('env_bool', true) !== false) throw new Error('env boolean setting was not parsed');
if (utils.settingBool('alpha', true) !== true) throw new Error('invalid boolean did not use fallback');

if (utils.settingList('list').join('|') !== 'json|items') throw new Error('JSON list setting was not parsed');
if (utils.settingList('env_list').join('|') !== 'one|two|three') throw new Error('env list setting was not split/trimmed');
if (utils.settingList('missing_list', ['fallback']).join('|') !== 'fallback') throw new Error('missing list did not use fallback');

if (utils.settingJson('object').source !== 'json') throw new Error('JSON object setting was not preserved');
if (utils.settingJson('json_text').source !== 'json-string') throw new Error('JSON string setting was not parsed');
if (utils.settingJson('env_json').source !== 'env') throw new Error('env JSON setting was not parsed');
if (utils.settingJson('alpha', { fallback: true }).fallback !== true) throw new Error('invalid JSON did not use fallback');

if (utils.expandHome('~/project', { homeDir: '/home/test' }) !== '/home/test/project') throw new Error('home path was not expanded');
if (utils.resolvePath('relative_path', { baseDir: '/tmp/base' }) !== '/tmp/base/relative_path') throw new Error('relative path was not resolved from baseDir');
if (utils.resolvePath(utils.setting('relative_path'), { baseDir: process.env.HOMEBOY_COMPONENT_PATH }) !== join(process.env.HOMEBOY_COMPONENT_PATH, 'reports/out.json')) throw new Error('setting path was not resolved from component path');

if (utils.metric('12.5') !== 12.5) throw new Error('metric did not coerce numeric string');
if (utils.metric('nan', 3) !== 3) throw new Error('metric did not use fallback for NaN');
if (utils.metric(Number.POSITIVE_INFINITY, 4) !== 4) throw new Error('metric did not use fallback for Infinity');

const generatedRunId = utils.runId('Scenario Name', { timestamp: 123, nonce: 'nonce' });
if (!/^json-namespace-Scenario-Name-\d+-123-nonce$/.test(generatedRunId)) throw new Error(`run id was not stable/sanitized: ${generatedRunId}`);

const successful = await utils.runCommand('node', [
  '-e',
  'console.log("token=abc keep=visible"); console.error("Authorization: Bearer secret")',
]);
if (successful.code !== 0) throw new Error('successful command returned non-zero');
if (successful.stdout.includes('abc') || !successful.stdout.includes('keep=visible')) throw new Error('stdout was not safely redacted');
if (successful.stderr.includes('secret')) throw new Error('stderr bearer token leaked');

const raw = await utils.runCommand('node', ['-e', 'console.log("token=abc")'], { redact: false });
if (!raw.stdout.includes('token=abc')) throw new Error('raw subprocess output was not available when requested');

const failed = await utils.runCommand('node', ['-e', 'console.error("password=hunter2"); process.exit(7)'], { allowFailure: true });
if (failed.code !== 7) throw new Error('failure result did not preserve exit code');
if (failed.stderr.includes('hunter2')) throw new Error('allowFailure stderr was not redacted');

let rejected = false;
try {
  await utils.runCommand('node', ['-e', 'console.error("api_key=secret"); process.exit(9)']);
} catch (error) {
  rejected = true;
  if (error.message.includes('secret')) throw new Error('rejected subprocess error leaked secret');
}
if (!rejected) throw new Error('non-zero command did not reject by default');

let timedOut = false;
try {
  await utils.runCommand('node', ['-e', 'console.log("token=abc"); setTimeout(() => {}, 2000)'], { timeoutMs: 50 });
} catch (error) {
  timedOut = true;
  if (!error.message.includes('timed out')) throw new Error('timeout error did not identify timeout');
  if (error.message.includes('abc')) throw new Error('timeout error leaked stdout secret');
}
if (!timedOut) throw new Error('timeout command did not reject');

const jsonFile = join(process.env.HOMEBOY_COMPONENT_PATH, 'artifacts', 'result.json');
await utils.writeJson(jsonFile, {
  ok: true,
  elapsed_ms: Number.POSITIVE_INFINITY,
  url: 'https://example.test/?token=abc&phase=load',
  password: 'secret',
});
const json = JSON.parse(await readFile(jsonFile, 'utf8'));
if (json.elapsed_ms !== null) throw new Error('writeJson did not normalize non-finite numbers');
if (json.url.includes('abc') || json.password !== '[REDACTED]') throw new Error('writeJson did not redact secrets');
if (!json.url.includes('phase=load')) throw new Error('writeJson removed safe query context');

const textFile = join(process.env.HOMEBOY_COMPONENT_PATH, 'artifacts', 'stdout.txt');
await utils.writeText(textFile, 'password=hunter2 keep=visible');
const text = await readFile(textFile, 'utf8');
if (text.includes('hunter2') || !text.includes('keep=visible')) throw new Error('writeText did not redact text safely');

if (utils.percentile([], 95) !== 0) throw new Error('empty percentile should be 0');
if (utils.percentile([5], 95) !== 5) throw new Error('single percentile should return only value');
if (utils.percentile([1, 2, 3, 4], 50) !== 2.5) throw new Error('p50 interpolation failed');
if (utils.percentile([1, 2, 3, 4], 95) !== 3.8499999999999996) throw new Error('p95 interpolation failed');
if (utils.percentile([1, Number.NaN, 3], 50) !== 2) throw new Error('percentile did not ignore non-finite values');

const context = utils.createArtifactContext({ id: 'Utility Smoke', runId: 'utility-run', artifactsDir: join(process.env.HOMEBOY_COMPONENT_PATH, 'context-artifacts') });
const descriptor = await context.writeJson('raw result', { token: 'abc', keep: 'visible' });
if (descriptor.kind !== 'json') throw new Error('artifact context wrapper did not write JSON artifact');
EOF

echo "Node.js workload utils smoke passed."
