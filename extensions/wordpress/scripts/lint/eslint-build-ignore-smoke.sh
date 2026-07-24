#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

component_dir="${TMPDIR}/component"
mkdir -p "${component_dir}/build" "${component_dir}/nested/build" "${component_dir}/nested/src"

printf '%s\n' 'const = rootBuild;' > "${component_dir}/build/app.js"
printf '%s\n' 'const = nestedBuild;' > "${component_dir}/nested/build/app.js"
printf '%s\n' 'export {};' > "${component_dir}/nested/src/app.js"

run_eslint() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component_dir" \
    HOMEBOY_COMPONENT_TEXT_DOMAIN="component" \
        bash "${SCRIPT_DIR}/eslint-runner.sh"
}

run_eslint > "${TMPDIR}/build-only.out"

printf '%s\n' 'const = source;' > "${component_dir}/nested/src/app.js"
if run_eslint > "${TMPDIR}/invalid-source.out" 2>&1; then
    echo "Expected sibling source to remain linted" >&2
    exit 1
fi

if ! grep -Fq "nested/src/app.js" "${TMPDIR}/invalid-source.out"; then
    echo "Expected ESLint failure to identify nested/src/app.js" >&2
    sed 's/^/  /' "${TMPDIR}/invalid-source.out" >&2
    exit 1
fi

echo "ESLint nested build ignore smoke passed"
