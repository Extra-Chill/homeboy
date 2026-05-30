#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_PATH}/../.." && pwd)/homeboy}"
SIDECAR_WRITER_HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-${HOMEBOY_CORE_DIR}/src/core/extension/runtime/sidecar-writer.sh}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
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
