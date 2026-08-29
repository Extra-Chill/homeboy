#!/usr/bin/env bash
set -euo pipefail

# Smoke: the Node.js test runner must not invoke bare `node --test` on
# TypeScript test files. Bare Node cannot resolve the `.js` specifiers TS
# sources import and fails with ERR_MODULE_NOT_FOUND before any test runs, which
# must not be reported as a product test failure. (#8324)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_PATH}/../.." && pwd)/homeboy}"
REPOSITORY_ROOT="$(cd "${EXTENSION_PATH}/.." && pwd)"
# Helpers resolve through the shared resolver. The literal path used
# here previously pointed at src/core/extension/runtime, a Homeboy layout
# that has not existed since the move to crates/.
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)" || exit 1
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
COMMAND_CAPTURE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || exit 1
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

for helper in "$SIDECAR_WRITER_HELPER" "$RUNNER_PRELUDE_HELPER" "$COMMAND_CAPTURE_HELPER"; do
    if [ ! -f "$helper" ]; then
        echo "Missing runtime helper: $helper" >&2
        exit 1
    fi
done

# A project with a TypeScript test and no `test`/targeted package script, so the
# runner reaches its built-in fallback.
PROJECT="${TMPDIR}/project"
mkdir -p "${PROJECT}/tests" "${PROJECT}/src"
cat > "${PROJECT}/package.json" <<'JSON'
{
  "name": "ts-test-fallback-smoke",
  "private": true,
  "scripts": {
    "build": "node -e \"process.exit(0)\""
  }
}
JSON
echo 'export const value = 1;' > "${PROJECT}/src/index.ts"
cat > "${PROJECT}/tests/example.test.ts" <<'TS'
import { test } from 'node:test';
import assert from 'node:assert';
import { value } from '../src/index.js';
test('value is one', () => assert.equal(value, 1));
TS

set +e
OUTPUT="$(
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PROJECT" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_CHANGED_TEST_FILES="tests/example.test.ts" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" 2>&1
)"
STATUS=$?
set -e

# The runner must never resolve bare `node --test` for a TypeScript file.
if printf '%s\n' "$OUTPUT" | grep -Eq "Command:[[:space:]]+node --test( |$)"; then
    echo "Runner invoked bare 'node --test' on a TypeScript test file" >&2
    printf '%s\n' "$OUTPUT" | sed 's/^/  /' >&2
    exit 1
fi

# Acceptable outcomes: a tsx-backed built-in command, or a clear actionable
# refusal. Both keep bare Node from misclassifying a loader error as a failure.
if printf '%s\n' "$OUTPUT" | grep -Fq "Command:   node --import tsx --test"; then
    echo "PASS: TypeScript tests routed through the tsx loader"
elif [ "$STATUS" -ne 0 ] && printf '%s\n' "$OUTPUT" | grep -Fq "require a declared test runner or a 'tsx' loader"; then
    echo "PASS: TypeScript tests without a loader fail with actionable guidance"
else
    echo "Unexpected TypeScript test resolution (status $STATUS):" >&2
    printf '%s\n' "$OUTPUT" | sed 's/^/  /' >&2
    exit 1
fi
