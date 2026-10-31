#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-package-script-bench.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

PROJECT_DIR="$TMP_DIR/project"
mkdir -p "$PROJECT_DIR/specs"

cat > "$PROJECT_DIR/package.json" <<'JSON'
{
  "name": "package-script-bench-smoke",
  "scripts": {
    "bench:e2e": "node fake-e2e-runner.mjs"
  }
}
JSON

cat > "$PROJECT_DIR/fake-e2e-runner.mjs" <<'JS'
import { writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
writeFileSync('received-args.json', JSON.stringify(args));

if (args.join('\n') !== '--project=chromium\nspecs/editor.spec.ts') {
  console.error(`Unexpected args: ${JSON.stringify(args)}`);
  process.exit(1);
}

console.log('fake e2e passed token=abc');
console.error('Authorization: Bearer secret');
JS

touch "$PROJECT_DIR/specs/editor.spec.ts"

HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_BENCH_ARTIFACTS_DIR="$TMP_DIR/artifacts" \
WORKLOAD_UTILS_UNDER_TEST="$SCRIPT_DIR/lib/workload-utils.mjs" \
node --input-type=module - <<'EOF'
import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';

const { runPackageScriptBench } = await import(process.env.WORKLOAD_UTILS_UNDER_TEST);

const result = await runPackageScriptBench({
  id: 'fake-e2e',
  script: 'bench:e2e',
  args: ['--project=chromium'],
  specs: ['specs/editor.spec.ts'],
  runId: 'package-script-smoke',
});

assert.equal(result.metrics.package_script_exit_code, 0);
assert.equal(result.metrics.package_script_spec_count, 1);
assert.equal(result.metadata.package_manager, 'npm');
assert.equal(result.metadata.package_script, 'bench:e2e');
assert.equal(result.metadata.package_script_arg_count, 2);
assert.ok(result.metrics.package_script_elapsed_ms >= 0);
assert.ok(result.metrics.package_script_stdout_bytes > 0);
assert.ok(result.metrics.package_script_stderr_bytes > 0);

const artifact = result.artifacts['package-script-result'];
assert.equal(artifact.kind, 'json');
assert.equal(artifact.label, 'Package script bench:e2e result');

const payload = JSON.parse(await readFile(artifact.path, 'utf8'));
assert.equal(payload.script, 'bench:e2e');
assert.deepEqual(payload.args, ['run', 'bench:e2e', '--', '--project=chromium', 'specs/editor.spec.ts']);
assert.deepEqual(payload.specs, ['specs/editor.spec.ts']);
assert.equal(payload.code, 0);
assert.match(payload.stdout, /fake e2e passed/);
assert.doesNotMatch(payload.stdout, /abc/);
assert.doesNotMatch(payload.stderr, /secret/);

const receivedArgs = JSON.parse(await readFile(`${process.env.HOMEBOY_COMPONENT_PATH}/received-args.json`, 'utf8'));
assert.deepEqual(receivedArgs, ['--project=chromium', 'specs/editor.spec.ts']);

let missingScriptFailed = false;
try {
  await runPackageScriptBench({ script: 'missing' });
} catch (error) {
  missingScriptFailed = true;
  assert.match(error.message, /package script "missing" is not defined/);
}
assert.equal(missingScriptFailed, true);
EOF

echo "Node.js package script bench smoke passed."
