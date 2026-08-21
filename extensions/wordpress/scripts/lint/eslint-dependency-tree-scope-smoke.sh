#!/usr/bin/env bash
# ESLint discovery must ignore the installed dependency tree.
#
# The JavaScript file count is a gate, not a metric: zero means skip ESLint
# entirely. Counting dependency-tree files makes every component that has
# dependencies installed look like it ships JavaScript of its own, so the skip
# never fires and a PHP-only component pays for a pointless ESLint run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REAL_EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_PATH="${TMPDIR}/extension"
component_dir="${TMPDIR}/component"
eslint_log="${TMPDIR}/eslint-args.txt"

mkdir -p "${EXTENSION_PATH}/node_modules/.bin"
touch "${EXTENSION_PATH}/eslint.runner.config.mjs"

cat > "${EXTENSION_PATH}/node_modules/.bin/eslint" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" > "${ESLINT_LOG}"
printf '[]\n'
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/eslint"

run_eslint_runner() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component_dir" \
    HOMEBOY_COMPONENT_TEXT_DOMAIN="component" \
    HOMEBOY_SUMMARY_MODE=1 \
    ESLINT_LOG="$eslint_log" \
        bash "${REAL_EXTENSION_PATH}/scripts/lint/eslint-runner.sh" > "$1"
}

# A PHP-only component that has its build dependencies installed. Every JS file
# present belongs to the dependency tree, including nested trees.
mkdir -p \
    "${component_dir}/inc" \
    "${component_dir}/node_modules/example-package" \
    "${component_dir}/inc/blocks/example/node_modules/nested-package"
printf '%s\n' '<?php' > "${component_dir}/inc/Thing.php"
printf '%s\n' 'module.exports = {};' > "${component_dir}/node_modules/example-package/index.js"
printf '%s\n' 'export const nested = 1;' \
    > "${component_dir}/inc/blocks/example/node_modules/nested-package/index.js"

rm -f "$eslint_log"
run_eslint_runner "${TMPDIR}/dependency-only.out"

if ! grep -Fq "No JavaScript files found, skipping ESLint." "${TMPDIR}/dependency-only.out"; then
    echo "FAIL: dependency-tree JavaScript must not satisfy the ESLint discovery gate" >&2
    sed 's/^/  /' "${TMPDIR}/dependency-only.out" >&2
    exit 1
fi

if [ -f "$eslint_log" ]; then
    echo "FAIL: ESLint ran for a component whose only JavaScript is installed dependencies" >&2
    sed 's/^/  /' "$eslint_log" >&2
    exit 1
fi

# The gate must still open for JavaScript the component actually owns.
mkdir -p "${component_dir}/assets"
printf '%s\n' 'const answer = 42;' > "${component_dir}/assets/app.js"

rm -f "$eslint_log"
run_eslint_runner "${TMPDIR}/component-js.out"

if ! grep -Fq "Running JavaScript linting..." "${TMPDIR}/component-js.out"; then
    echo "FAIL: component-owned JavaScript should still be linted" >&2
    sed 's/^/  /' "${TMPDIR}/component-js.out" >&2
    exit 1
fi

if [ ! -f "$eslint_log" ]; then
    echo "FAIL: ESLint should have run for component-owned JavaScript" >&2
    sed 's/^/  /' "${TMPDIR}/component-js.out" >&2
    exit 1
fi

echo "ESLint dependency tree scope smoke passed"
