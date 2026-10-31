#!/usr/bin/env bash
set -euo pipefail

# Regression: functions declared by a Composer-installed WordPress plugin must
# be visible to PHPStan.
#
# A plugin pulled in through Composer (`"type": "wordpress-plugin"`) usually
# lists only its bootstrap in `autoload.files`. The bootstrap returns early
# outside WordPress, before its `require_once` chain runs, and the package's
# classmap covers classes only. The functions in the required files were
# therefore invisible and every real call from component source reported
# `Function ... not found` (Extra-Chill/data-machine#3452).
#
# The fixture installs such a package under vendor/ with a guarded bootstrap
# and a namespaced function in a require_once'd file, and calls that function
# from component source. It must analyse clean.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

COMPONENT_DIR="${TMP_ROOT}/component"
PACKAGE_DIR="${COMPONENT_DIR}/vendor/acme/substrate"
CONFIG_TMPDIR="${TMP_ROOT}/generated-config"
OUTPUT_FILE="${TMP_ROOT}/phpstan-output.txt"
RESOLVE_CONTEXT_HELPER="${TMP_ROOT}/resolve-context.sh"
SIDECAR_WRITER_HELPER="${TMP_ROOT}/sidecar-writer.sh"

mkdir -p "${COMPONENT_DIR}/inc" "${PACKAGE_DIR}/src/Workflows" "${COMPONENT_DIR}/vendor/composer" "$CONFIG_TMPDIR"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpstan-composer-plugin-functions}"
}
SH

cat > "$SIDECAR_WRITER_HELPER" <<'SH'
homeboy_sidecar_merge_json_array() {
    return 0
}
SH

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPStan Composer Plugin Functions Fixture
 */
PHP

# The vendored plugin: guarded bootstrap, function file behind require_once.
cat > "${PACKAGE_DIR}/substrate.php" <<'PHP'
<?php
if ( ! defined( 'ABSPATH' ) ) {
    return;
}
require_once __DIR__ . '/src/Workflows/register-runtime.php';
PHP

cat > "${PACKAGE_DIR}/src/Workflows/register-runtime.php" <<'PHP'
<?php
namespace Acme\Substrate\Workflows;

function register_fixture_runtime( string $runtime, callable $handler ): bool {
    return '' !== $runtime && is_callable( $handler );
}
PHP

cat > "${COMPONENT_DIR}/vendor/composer/installed.json" <<'JSON'
{
    "packages": [
        {
            "name": "acme/substrate",
            "type": "wordpress-plugin",
            "install-path": "../acme/substrate",
            "autoload": { "files": [ "substrate.php" ] }
        }
    ]
}
JSON

cat > "${COMPONENT_DIR}/vendor/autoload.php" <<'PHP'
<?php
require_once __DIR__ . '/acme/substrate/substrate.php';
PHP

cat > "${COMPONENT_DIR}/inc/runtime.php" <<'PHP'
<?php

function fixture_register(): bool {
    return \Acme\Substrate\Workflows\register_fixture_runtime(
        'fixture',
        static function (): void {}
    );
}
PHP

if [ ! -x "${ROOT_DIR}/vendor/bin/phpstan" ]; then
    echo "FAIL: PHPStan is not installed. Run composer install in ${ROOT_DIR}." >&2
    exit 1
fi

# Unit check of the resolver.
eval "$(awk '/^homeboy_resolve_phpstan_composer_plugin_function_files\(\)/,/^}/' "$PHPSTAN_RUNNER")"
resolved="$(homeboy_resolve_phpstan_composer_plugin_function_files "$COMPONENT_DIR")"
if ! grep -F "src/Workflows/register-runtime.php" <<< "$resolved" >/dev/null; then
    echo "FAIL: the plugin's function file must be pinned as a scanFiles entry" >&2
    printf '%s\n' "$resolved" >&2
    exit 1
fi
if grep -F "substrate.php" <<< "$resolved" >/dev/null; then
    echo "FAIL: files that declare no top-level function must not be pinned" >&2
    exit 1
fi

for scope in full scoped; do
    set +e
    if [ "$scope" = scoped ]; then
        lint_file="inc/runtime.php"
    else
        lint_file=""
    fi
    HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="phpstan-composer-plugin-functions" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    HOMEBOY_SUMMARY_MODE=1 \
    HOMEBOY_PHPSTAN_THREADS=1 \
    HOMEBOY_LINT_FILE="$lint_file" \
    TMPDIR="$CONFIG_TMPDIR" \
    bash -c 'cd "$1" && "$2"' _ "$CONFIG_TMPDIR" "$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
    exit_code=$?
    set -e

    if grep -q 'register_fixture_runtime not found' "$OUTPUT_FILE"; then
        echo "FAIL (${scope}): a Composer-installed plugin's function was invisible to PHPStan" >&2
        cat "$OUTPUT_FILE" >&2
        exit 1
    fi
    if [ "$exit_code" -ne 0 ]; then
        echo "FAIL (${scope}): analysis should succeed" >&2
        cat "$OUTPUT_FILE" >&2
        exit 1
    fi
done

echo "PASS: Composer-installed WordPress plugin functions are visible to PHPStan"
