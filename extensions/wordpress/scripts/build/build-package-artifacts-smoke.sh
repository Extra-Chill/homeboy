#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-package-artifacts.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

assert_zip_contains() {
    local zip_file="$1"
    local expected="$2"
    if ! unzip -l "$zip_file" | grep -F -- "$expected" >/dev/null; then
        echo "Expected $zip_file to contain $expected" >&2
        unzip -l "$zip_file" >&2
        exit 1
    fi
}

assert_zip_not_contains() {
    local zip_file="$1"
    local unexpected="$2"
    if unzip -l "$zip_file" | grep -F -- "$unexpected" >/dev/null; then
        echo "Expected $zip_file not to contain $unexpected" >&2
        unzip -l "$zip_file" >&2
        exit 1
    fi
}

component_dir="${TMP_DIR}/package-artifact-plugin"
mkdir -p "${component_dir}/runtime/packages" "${component_dir}/runtime/nested/packages" "${component_dir}/runtime/omit" "${component_dir}/includes"
mkdir -p "${component_dir}/.git"

cat > "${component_dir}/package-artifact-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Package Artifact Plugin
 * Version: 1.0.0
 */
PHP

printf '%s\n' '<?php' > "${component_dir}/includes/bootstrap.php"
printf '%s\n' 'accidental release zip' > "${component_dir}/accidental.zip"
printf '%s\n' 'intentional package zip' > "${component_dir}/runtime/packages/static-site-importer.zip"
printf '%s\n' 'nested package zip' > "${component_dir}/runtime/nested/packages/nested.zip"
printf '%s\n' 'excluded runtime source' > "${component_dir}/runtime/omit/source.txt"
printf '%s\n' 'ref: refs/heads/main' > "${component_dir}/.git/HEAD"
ln -s "${component_dir}/runtime/packages/static-site-importer.zip" "${component_dir}/runtime/packages/linked.zip"

(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/default-build.out"
)

default_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_contains "$default_zip" "package-artifact-plugin/package-artifact-plugin.php"
assert_zip_not_contains "$default_zip" "package-artifact-plugin/accidental.zip"
assert_zip_not_contains "$default_zip" "package-artifact-plugin/runtime/packages/static-site-importer.zip"
assert_zip_not_contains "$default_zip" "package-artifact-plugin/.git/HEAD"

(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["runtime/**/packages/*.zip","runtime/packages/*.zip"],"package_excludes":["/runtime/omit/"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/included-build.out"
)

included_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_contains "$included_zip" "package-artifact-plugin/runtime/packages/static-site-importer.zip"
assert_zip_contains "$included_zip" "package-artifact-plugin/runtime/nested/packages/nested.zip"
assert_zip_not_contains "$included_zip" "package-artifact-plugin/accidental.zip"
assert_zip_not_contains "$included_zip" "package-artifact-plugin/runtime/omit/source.txt"
assert_zip_not_contains "$included_zip" "package-artifact-plugin/runtime/packages/linked.zip"

if ! grep -Fq '"type":"wordpress.package_artifacts"' "${TMP_DIR}/included-build.out"; then
    echo "Expected structured package artifact output" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

if ! grep -Fq '"path":"runtime/packages/static-site-importer.zip"' "${TMP_DIR}/included-build.out"; then
    echo "Expected included artifact path in structured output" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

if ! grep -Eq '"sha256":"[0-9a-f]{64}"' "${TMP_DIR}/included-build.out"; then
    echo "Expected included artifact SHA-256 in structured output" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

if [ "$(grep -Fc '"path":"runtime/packages/static-site-importer.zip"' "${TMP_DIR}/included-build.out")" -ne 1 ]; then
    echo "Expected duplicate artifact patterns to emit one manifest entry" >&2
    sed 's/^/  /' "${TMP_DIR}/included-build.out" >&2
    exit 1
fi

mkdir -p "${component_dir}/.homeboy-build/stale/packages"
printf '%s\n' 'stale staging zip' > "${component_dir}/.homeboy-build/stale/packages/stale.zip"
(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["**/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/staging-build.out"
)

staging_zip="${component_dir}/build/package-artifact-plugin.zip"
assert_zip_not_contains "$staging_zip" "package-artifact-plugin/.homeboy-build/stale/packages/stale.zip"

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["runtime/missing/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/missing-build.out" 2>&1
); then
    echo "Expected missing declared package artifact pattern to fail" >&2
    sed 's/^/  /' "${TMP_DIR}/missing-build.out" >&2
    exit 1
fi

if ! grep -Fq 'Declared WordPress package artifact pattern matched no files: runtime/missing/*.zip' "${TMP_DIR}/missing-build.out"; then
    echo "Expected missing artifact error" >&2
    sed 's/^/  /' "${TMP_DIR}/missing-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["../outside/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/traversal-build.out" 2>&1
); then
    echo "Expected traversal artifact pattern to fail" >&2
    exit 1
fi

if ! grep -Fq "cannot contain '..'" "${TMP_DIR}/traversal-build.out"; then
    echo "Expected traversal artifact error" >&2
    sed 's/^/  /' "${TMP_DIR}/traversal-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_artifacts":["/tmp/*.zip"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/absolute-build.out" 2>&1
); then
    echo "Expected absolute artifact pattern to fail" >&2
    exit 1
fi

if ! grep -Fq 'must be component-relative' "${TMP_DIR}/absolute-build.out"; then
    echo "Expected absolute artifact error" >&2
    sed 's/^/  /' "${TMP_DIR}/absolute-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_excludes":["../outside/"]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/exclude-traversal-build.out" 2>&1
); then
    echo "Expected traversal package exclude to fail" >&2
    exit 1
fi

if ! grep -Fq 'Package excludes cannot contain traversal' "${TMP_DIR}/exclude-traversal-build.out"; then
    echo "Expected traversal package exclude error" >&2
    sed 's/^/  /' "${TMP_DIR}/exclude-traversal-build.out" >&2
    exit 1
fi

if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="package-artifact-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON='{"package_excludes":[42]}' \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/exclude-type-build.out" 2>&1
); then
    echo "Expected non-string package exclude to fail" >&2
    exit 1
fi

if ! grep -Fq 'package_excludes entries must be non-empty strings' "${TMP_DIR}/exclude-type-build.out"; then
    echo "Expected package exclude type error" >&2
    sed 's/^/  /' "${TMP_DIR}/exclude-type-build.out" >&2
    exit 1
fi

echo "WordPress package artifacts build smoke passed."
