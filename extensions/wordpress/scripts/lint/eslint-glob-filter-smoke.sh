#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REAL_EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
EXTENSION_PATH="${TMPDIR}/extension"

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

component_dir="${TMPDIR}/component"
findings_file="${TMPDIR}/eslint-findings.json"
mkdir -p "${component_dir}/inc" "${component_dir}/assets" "${EXTENSION_PATH}/node_modules/.bin"
touch "${EXTENSION_PATH}/eslint.config.mjs"
printf '%s\n' '<?php' > "${component_dir}/inc/Thing.php"
printf '%s\n' 'const answer = 42;' > "${component_dir}/assets/app.js"

eslint_log="${TMPDIR}/eslint-args.txt"
cat > "${EXTENSION_PATH}/node_modules/.bin/eslint" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" > "${ESLINT_LOG}"
printf '[]\n'
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/eslint"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component_dir" \
HOMEBOY_COMPONENT_TEXT_DOMAIN="component" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_LINT_GLOB="{${component_dir}/inc/Thing.php}" \
ESLINT_LOG="$eslint_log" \
    bash "${REAL_EXTENSION_PATH}/scripts/lint/eslint-runner.sh" > "${TMPDIR}/php-only.out"

if [ -f "$eslint_log" ]; then
    echo "Expected ESLint not to run for PHP-only glob" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component_dir" \
HOMEBOY_COMPONENT_TEXT_DOMAIN="component" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_LINT_GLOB="{${component_dir}/inc/Thing.php,${component_dir}/assets/app.js}" \
HOMEBOY_LINT_FINDINGS_FILE="$findings_file" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
ESLINT_LOG="$eslint_log" \
    bash "${REAL_EXTENSION_PATH}/scripts/lint/eslint-runner.sh" > "${TMPDIR}/mixed.out"

assert_contains "${TMPDIR}/mixed.out" "Linting 1 JS/TS files matching"
assert_contains "$eslint_log" "${component_dir}/assets/app.js"
assert_not_contains "$eslint_log" "${component_dir}/inc/Thing.php"

python3 - "$findings_file" <<'PY'
import json
import sys

assert json.load(open(sys.argv[1], encoding="utf-8")) == []
PY

echo "ESLint glob filter smoke passed"
