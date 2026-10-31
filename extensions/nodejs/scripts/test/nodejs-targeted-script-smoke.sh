#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_DIR}/../.." && pwd)/homeboy}"
REPOSITORY_ROOT="$(cd "${EXTENSION_DIR}/.." && pwd)"
# Helpers resolve through the shared resolver. The literal path used
# here previously pointed at src/core/extension/runtime, a Homeboy layout
# that has not existed since the move to crates/.
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
COMMAND_CAPTURE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || exit 1

if [ ! -f "$RUNNER_PRELUDE_HELPER" ]; then
    echo "Missing runner prelude helper: $RUNNER_PRELUDE_HELPER" >&2
    exit 1
fi
if [ ! -f "$COMMAND_CAPTURE_HELPER" ]; then
    echo "Missing command capture helper: $COMMAND_CAPTURE_HELPER" >&2
    exit 1
fi

TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-targeted.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

run_declared_targeted_script_smoke() {
    local project_dir="${TMPDIR}/declared"
    mkdir -p "$project_dir/packages/core-data/src/test"

    cat > "${project_dir}/package.json" <<'JSON'
{
  "name": "declared-targeted-script",
  "scripts": {
    "test": "node aggregate-test-should-not-run.mjs",
    "test:unit": "node unit-test-recorder.mjs"
  }
}
JSON

    cat > "${project_dir}/aggregate-test-should-not-run.mjs" <<'JS'
console.error('aggregate test script should not run for targeted Gutenberg args');
process.exit(1);
JS

    cat > "${project_dir}/unit-test-recorder.mjs" <<'JS'
import { writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
writeFileSync('received-args.json', JSON.stringify(args));

if (args.join('\n') !== 'packages/core-data/src/test/resolvers.js\n--runInBand') {
  console.error(`Unexpected test args: ${JSON.stringify(args)}`);
  process.exit(1);
}

console.log('PASS packages/core-data/src/test/resolvers.js');
console.log('Tests:       36 passed, 36 total');
JS

    touch "${project_dir}/packages/core-data/src/test/resolvers.js"

    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$project_dir" \
    HOMEBOY_COMPONENT_ID="declared-targeted-smoke" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_SETTINGS_JSON='{"test_script":"test:unit"}' \
    bash "${SCRIPT_DIR}/test-runner.sh" packages/core-data/src/test/resolvers.js --runInBand > "${TMPDIR}/declared.out"

    if ! grep -q 'Command:   npm run test:unit --' "${TMPDIR}/declared.out"; then
        echo "Expected declared targeted args to use test:unit" >&2
        cat "${TMPDIR}/declared.out" >&2
        exit 1
    fi

    if [ "$(cat "${project_dir}/received-args.json")" != '["packages/core-data/src/test/resolvers.js","--runInBand"]' ]; then
        echo "Runner did not forward targeted Gutenberg args to test:unit" >&2
        cat "${project_dir}/received-args.json" >&2
        exit 1
    fi
}

run_configured_script_smoke() {
    local project_dir="${TMPDIR}/configured"
    mkdir -p "$project_dir/tests"

    cat > "${project_dir}/package.json" <<'JSON'
{
  "name": "configured-targeted-script",
  "scripts": {
    "test": "node aggregate-test-should-not-run.mjs",
    "test:unit": "node unit-test-recorder.mjs"
  }
}
JSON

    cat > "${project_dir}/aggregate-test-should-not-run.mjs" <<'JS'
console.error('aggregate test script should not run when targeted test_script is configured');
process.exit(1);
JS

    cat > "${project_dir}/unit-test-recorder.mjs" <<'JS'
import { writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
writeFileSync('received-args.json', JSON.stringify(args));

if (args.join('\n') !== 'tests/example.test.js') {
  console.error(`Unexpected test args: ${JSON.stringify(args)}`);
  process.exit(1);
}

console.log('# tests 1');
console.log('# pass 1');
console.log('# fail 0');
JS

    touch "${project_dir}/tests/example.test.js"

    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$project_dir" \
    HOMEBOY_COMPONENT_ID="configured-targeted-smoke" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_SETTINGS_JSON='{"test_script":"test:unit"}' \
    bash "${SCRIPT_DIR}/test-runner.sh" tests/example.test.js > "${TMPDIR}/configured.out"

    if ! grep -q 'Command:   npm run test:unit --' "${TMPDIR}/configured.out"; then
        echo "Expected configured targeted args to use test:unit" >&2
        cat "${TMPDIR}/configured.out" >&2
        exit 1
    fi

    if [ "$(cat "${project_dir}/received-args.json")" != '["tests/example.test.js"]' ]; then
        echo "Runner did not forward configured targeted args to test:unit" >&2
        cat "${project_dir}/received-args.json" >&2
        exit 1
    fi
}

run_declared_targeted_script_smoke
run_configured_script_smoke

echo "nodejs targeted-script smoke passed"
