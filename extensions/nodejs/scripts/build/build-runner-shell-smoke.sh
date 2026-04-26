#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MANIFEST="${EXTENSION_DIR}/nodejs.json"
RUNNER="${EXTENSION_DIR}/scripts/build/build-runner.sh"

node - "$MANIFEST" "$RUNNER" <<'NODE'
const fs = require('fs');
const [manifestPath, runnerPath] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
const runner = fs.readFileSync(runnerPath, 'utf8');

function assert(condition, message) {
  if (!condition) {
    console.error(`FAIL: ${message}`);
    process.exitCode = 1;
  }
}

assert(manifest.build.extension_script === 'scripts/build/build-runner.sh', 'build runner path is stable');
assert(manifest.build.command_template === 'bash {{script}}', 'build runner executes through bash');
assert(runner.startsWith('#!/usr/bin/env bash'), 'build runner declares bash shebang');
assert(runner.includes('set -euo pipefail'), 'build runner uses bash pipefail mode');
assert(runner.includes('BASH_SOURCE[0]'), 'build runner uses bash BASH_SOURCE');
assert(runner.includes('PIPESTATUS[0]'), 'build runner uses bash PIPESTATUS');

if (process.exitCode) {
  process.exit();
}

console.log('Node.js build runner shell smoke passed');
NODE
