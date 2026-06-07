#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEPENDENCY_HELPER="${EXTENSION_DIR}/scripts/lib/validation-dependencies.sh"

# shellcheck source=../scripts/lib/validation-dependencies.sh
source "$DEPENDENCY_HELPER"

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-dependency-preflight-smoke.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT

assert_diagnostic() {
    local diagnostics_file="$1"
    local jq_filter="$2"
    local message="$3"

    if ! jq -e "$jq_filter" "$diagnostics_file" >/dev/null; then
        echo "ERROR: ${message}" >&2
        jq '.' "$diagnostics_file" >&2 || true
        exit 1
    fi
}

MISSING_PATH="${TMP_ROOT}/missing-plugin"
MISSING_PATH_ARTIFACTS="${TMP_ROOT}/missing-path-artifacts"
if homeboy_preflight_wordpress_dependency_plugins "$MISSING_PATH" "$MISSING_PATH_ARTIFACTS" "bench"; then
    echo "ERROR: expected missing dependency path preflight to fail." >&2
    exit 1
fi
assert_diagnostic \
    "${MISSING_PATH_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" \
    '(.diagnostics | length) == 1 and .diagnostics[0].code == "wordpress-dependency-path-missing" and .diagnostics[0].dependency_slug == "missing-plugin"' \
    'missing path diagnostic did not identify the missing dependency path.'

WOOCOMMERCE_SOURCE="${TMP_ROOT}/woocommerce"
mkdir -p "${WOOCOMMERCE_SOURCE}/plugins/woocommerce"
cat > "${WOOCOMMERCE_SOURCE}/plugins/woocommerce/woocommerce.php" <<'PHP'
<?php
/**
 * Plugin Name: WooCommerce
 */
PHP

MISSING_MAIN_ARTIFACTS="${TMP_ROOT}/missing-main-artifacts"
if homeboy_preflight_wordpress_dependency_plugins "$WOOCOMMERCE_SOURCE" "$MISSING_MAIN_ARTIFACTS" "bench"; then
    echo "ERROR: expected source checkout without root plugin main file to fail." >&2
    exit 1
fi
assert_diagnostic \
    "${MISSING_MAIN_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" \
    '(.diagnostics | length) == 1 and .diagnostics[0].code == "wordpress-dependency-plugin-main-file-missing" and .diagnostics[0].package_required == true and (.diagnostics[0].source_checkout_plugin_file | endswith("/plugins/woocommerce/woocommerce.php"))' \
    'missing main file diagnostic did not identify source-checkout package requirement.'

FATAL_DEP="${TMP_ROOT}/woocommerce-packaged"
mkdir -p "$FATAL_DEP"
cat > "${FATAL_DEP}/woocommerce-packaged.php" <<'PHP'
<?php
/**
 * Plugin Name: WooCommerce Packaged Fixture
 */
require_once __DIR__ . '/includes/react-admin/feature-config.php';
PHP

LOAD_FATAL_ARTIFACTS="${TMP_ROOT}/load-fatal-artifacts"
if homeboy_preflight_wordpress_dependency_plugins "$FATAL_DEP" "$LOAD_FATAL_ARTIFACTS" "bench"; then
    echo "ERROR: expected plugin load fatal preflight to fail." >&2
    exit 1
fi
assert_diagnostic \
    "${LOAD_FATAL_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" \
    '(.diagnostics | length) == 1 and .diagnostics[0].code == "wordpress-dependency-plugin-load-fatal" and .diagnostics[0].package_required == true and (.diagnostics[0].missing_include | endswith("/includes/react-admin/feature-config.php"))' \
    'load fatal diagnostic did not identify the missing build artifact.'

READY_DEP="${TMP_ROOT}/ready-plugin"
mkdir -p "$READY_DEP"
cat > "${READY_DEP}/ready-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Ready Plugin
 */
add_action('plugins_loaded', static function () {});
PHP

READY_ARTIFACTS="${TMP_ROOT}/ready-artifacts"
homeboy_preflight_wordpress_dependency_plugins "$READY_DEP" "$READY_ARTIFACTS" "bench"
if [ -f "${READY_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" ]; then
    echo "ERROR: ready plugin dependency should not emit preflight diagnostics." >&2
    exit 1
fi

NESTED_PACKAGE_DEP="${TMP_ROOT}/wp-codebox-release-fixture"
mkdir -p "${NESTED_PACKAGE_DEP}/packages/wordpress-plugin"
cat > "${NESTED_PACKAGE_DEP}/packages/wordpress-plugin/wp-codebox.php" <<'PHP'
<?php
/**
 * Plugin Name: WP Codebox Fixture
 */
PHP

NESTED_PACKAGE_ARTIFACTS="${TMP_ROOT}/nested-package-artifacts"
homeboy_preflight_wordpress_dependency_plugins "$NESTED_PACKAGE_DEP" "$NESTED_PACKAGE_ARTIFACTS" "bench"
if [ -f "${NESTED_PACKAGE_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" ]; then
    echo "ERROR: nested package plugin dependency should not emit preflight diagnostics." >&2
    jq '.' "${NESTED_PACKAGE_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" >&2 || true
    exit 1
fi

SANITIZE_KEY_DEP="${TMP_ROOT}/sanitize-key-plugin"
mkdir -p "$SANITIZE_KEY_DEP"
cat > "${SANITIZE_KEY_DEP}/sanitize-key-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Sanitize Key Fixture
 */
$fixture_key = sanitize_key('Fixture Key');
PHP

SANITIZE_KEY_ARTIFACTS="${TMP_ROOT}/sanitize-key-artifacts"
homeboy_preflight_wordpress_dependency_plugins "$SANITIZE_KEY_DEP" "$SANITIZE_KEY_ARTIFACTS" "bench"
if [ -f "${SANITIZE_KEY_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" ]; then
    echo "ERROR: sanitize_key plugin dependency should not emit preflight diagnostics." >&2
    jq '.' "${SANITIZE_KEY_ARTIFACTS}/wordpress-dependency-plugin-preflight-diagnostics.json" >&2 || true
    exit 1
fi

echo "dependency plugin preflight smoke passed"
