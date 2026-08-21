#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

component_dir="${TMPDIR}/component"
mkdir -p "${component_dir}/build" "${component_dir}/nested/build" "${component_dir}/nested/dist" "${component_dir}/nested/vendor/package" "${component_dir}/nested/src"

printf '%s\n' 'const = rootBuild;' > "${component_dir}/build/app.js"
printf '%s\n' 'const = nestedBuild;' > "${component_dir}/nested/build/app.js"
printf '%s\n' 'const = nestedDist;' > "${component_dir}/nested/dist/app.js"
printf '%s\n' 'const = nestedVendor;' > "${component_dir}/nested/vendor/package/app.js"
printf '%s\n' 'export {};' > "${component_dir}/nested/src/app.js"

# Reproduce the release failure's scale without placing generated bundles in
# the repository. These files must never become runner arguments or findings.
for index in $(seq 1 1500); do
    printf 'const = generated%s;\n' "$index" > "${component_dir}/nested/build/generated-${index}.js"
done

printf '%s\n' 'export default [];' > "${component_dir}/eslint.config.mjs"

run_eslint() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component_dir" \
    HOMEBOY_COMPONENT_TEXT_DOMAIN="component" \
        bash "${SCRIPT_DIR}/eslint-runner.sh"
}

run_package_eslint() {
    (
        cd "$component_dir"
        "${EXTENSION_PATH}/node_modules/.bin/eslint" \
            --config "${EXTENSION_PATH}/eslint.config.mjs" .
    )
}

run_eslint > "${TMPDIR}/build-only.out"

if grep -Fq "generated-" "${TMPDIR}/build-only.out" || [ "$(wc -l < "${TMPDIR}/build-only.out" | tr -d ' ')" -gt 10 ]; then
    echo "Expected generated-only lint output to stay bounded" >&2
    sed 's/^/  /' "${TMPDIR}/build-only.out" >&2
    exit 1
fi

# Package-style ESLint and the runner must agree: generated output is ignored,
# while an authored source violation remains a failure in both entry points.
if ! run_package_eslint > "${TMPDIR}/package-valid.out" 2>&1; then
    echo "Expected package-style ESLint to ignore generated output" >&2
    sed 's/^/  /' "${TMPDIR}/package-valid.out" >&2
    exit 1
fi

if ! HOMEBOY_LINT_GLOB='nested/build/generated-*.js' run_eslint > "${TMPDIR}/generated-scope.out" 2>&1; then
    echo "Expected generated-only changed scope to skip ESLint" >&2
    sed 's/^/  /' "${TMPDIR}/generated-scope.out" >&2
    exit 1
fi

if ! grep -Fq "No JS/TS files match pattern: nested/build/generated-*.js" "${TMPDIR}/generated-scope.out"; then
    echo "Expected generated-only changed scope to be filtered before ESLint" >&2
    sed 's/^/  /' "${TMPDIR}/generated-scope.out" >&2
    exit 1
fi

printf '%s\n' 'const = source;' > "${component_dir}/nested/src/app.js"
if run_package_eslint > "${TMPDIR}/package-invalid.out" 2>&1; then
    echo "Expected package-style ESLint to retain authored source findings" >&2
    exit 1
fi

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
