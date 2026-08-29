#!/usr/bin/env bash
set -euo pipefail

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

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi
if [ ! -f "$RUNNER_PRELUDE_HELPER" ]; then
    echo "Missing runner prelude helper: $RUNNER_PRELUDE_HELPER" >&2
    exit 1
fi
if [ ! -f "$COMMAND_CAPTURE_HELPER" ]; then
    echo "Missing command capture helper: $COMMAND_CAPTURE_HELPER" >&2
    exit 1
fi

PROJECT="${TMPDIR}/project"
mkdir -p "$PROJECT"
cat > "${PROJECT}/package.json" <<'JSON'
{
  "name": "typecheck-ci-smoke",
  "private": true,
  "scripts": {
    "typecheck": "node -e \"console.log('typecheck ran')\""
  }
}
JSON

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_PATH="$PROJECT" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_NODE_LINT_COMMAND="__homeboy_typecheck" \
    bash "${EXTENSION_PATH}/scripts/lint/lint-runner.sh" > "${TMPDIR}/typecheck.out"

if ! grep -Fq "Command:   npm run typecheck" "${TMPDIR}/typecheck.out"; then
    echo "Expected typecheck CI sentinel to resolve through package-manager runner" >&2
    sed 's/^/  /' "${TMPDIR}/typecheck.out" >&2
    exit 1
fi

if ! grep -Fq "typecheck ran" "${TMPDIR}/typecheck.out"; then
    echo "Expected typecheck script to run" >&2
    sed 's/^/  /' "${TMPDIR}/typecheck.out" >&2
    exit 1
fi

echo "Node.js typecheck CI command smoke passed"
