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
import { chmod, mkdir, readFile, writeFile } from 'node:fs/promises';
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

const sourceDir = join(process.env.HOMEBOY_COMPONENT_PATH, 'source-site');
await mkdir(sourceDir, { recursive: true });
await writeFile(join(sourceDir, 'index.html'), '<main>Source</main>');
const fakeCli = join(process.env.HOMEBOY_COMPONENT_PATH, 'fake-wp-codebox-cli.mjs');
await writeFile(fakeCli, `#!/usr/bin/env node
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const recipePath = process.argv[process.argv.indexOf('--recipe') + 1];
const recipe = JSON.parse(await readFile(recipePath, 'utf8'));
const compareStep = recipe.workflow.steps.find((step) => step.command === 'wordpress.visual-compare');
const args = compareStep.args;
const arg = (name) => args.find((item) => item.startsWith(name + '='))?.slice(name.length + 1);
const sourceResponse = await fetch(arg('source-url'));
if (!sourceResponse.ok) throw new Error('source URL was not served');
const dir = join(recipe.artifacts.directory, 'files/browser/visual-compare');
await mkdir(dir, { recursive: true });
await writeFile(join(dir, 'source.png'), 'source');
await writeFile(join(dir, 'candidate.png'), 'candidate');
await writeFile(join(dir, 'diff.png'), 'diff');
const visual = {
  schema: 'wp-codebox/visual-compare/v1',
  status: 'different',
  files: {
    sourceScreenshot: 'files/browser/visual-compare/source.png',
    candidateScreenshot: 'files/browser/visual-compare/candidate.png',
    diffScreenshot: 'files/browser/visual-compare/diff.png',
    visualDiff: 'files/browser/visual-compare/visual-diff.json',
    summary: 'files/browser/visual-compare/summary.json'
  },
  viewport: { width: 640, height: 480 },
  comparison: { mismatchPixels: 10, totalPixels: 1000, mismatchRatio: 0.01, dimensionMismatch: false, regions: [{ x: 1, y: 2, width: 3, height: 4 }] }
};
await writeFile(join(dir, 'visual-diff.json'), JSON.stringify(visual, null, 2));
await writeFile(join(dir, 'summary.json'), JSON.stringify(visual, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  commands: [{ artifact: { files: { visualDiff: 'files/browser/visual-compare/visual-diff.json' } } }]
}));
`);
await chmod(fakeCli, 0o755);

const visualResult = await utils.runVisualParityWorkload({
  id: 'Visual Contract',
  artifactsDir: join(process.env.HOMEBOY_COMPONENT_PATH, 'visual-artifacts'),
  runId: 'visual-run',
  wpCodeboxCli: fakeCli,
  source: { path: sourceDir, ref: 'source-ref', label: 'static-source', port: 48531 },
  candidate: {
    url: '/',
    ref: 'candidate-ref',
    label: 'candidate-wordpress',
    context: { runtime: 'playground' },
    recipe: { runtime: { wp: 'latest' }, inputs: { mounts: [] }, workflow: { steps: [{ command: 'wordpress.setup', args: [] }] } },
  },
  viewport: { width: 640, height: 480 },
  threshold: 0.02,
});
if (visualResult.metrics.visual_parity_pass !== 1) throw new Error('visual parity pass metric was not set');
if (visualResult.metrics.visual_parity_mismatch_ratio !== 0.01) throw new Error('visual parity mismatch ratio metric was not normalized');
const visualArtifact = JSON.parse(await readFile(visualResult.artifacts.visualParity.path, 'utf8'));
if (visualArtifact.schema !== 'homeboy/VisualParityArtifact/v1') throw new Error('visual artifact schema mismatch');
if (visualArtifact.source.ref !== 'source-ref' || visualArtifact.candidate.ref !== 'candidate-ref') throw new Error('visual artifact refs were not preserved');
if (visualArtifact.summary.status !== 'passed' || visualArtifact.summary.region_count !== 1) throw new Error('visual artifact summary was not normalized');
if (visualArtifact.artifacts.visual_diff !== 'files/browser/visual-compare/visual-diff.json') throw new Error('visual diff artifact ref was not preserved');
const recipeArtifact = JSON.parse(await readFile(visualResult.metadata.codebox_recipe, 'utf8'));
if (recipeArtifact.runtime.wp !== 'latest') throw new Error('candidate recipe runtime was not merged');
if (recipeArtifact.workflow.steps[0].command !== 'wordpress.setup') throw new Error('candidate setup step was not preserved before visual compare');
if (!recipeArtifact.workflow.steps[1].args.includes('viewport=640x480')) throw new Error('visual compare viewport was not forwarded');
EOF

echo "Node.js workload utils smoke passed."
