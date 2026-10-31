#!/usr/bin/env bash
set -euo pipefail

# Proves the core contract of homeboy_wordpress_apply_extension_wp_scripts_toolchain
# (homeboy-extensions#2870): a component that already vendors its own
# @wordpress/scripts is completely unaffected -- PATH, NODE_PATH, and
# HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG are left untouched, and the extension's
# copy is never consulted. Only when the component has no local wp-scripts
# binary does the extension's toolchain get applied. Also proves the
# companion detection helpers (declared-dependency vs. script-only
# invocation) classify fixtures correctly.
#
# This is a fast, library-level test (no real npm/webpack work) -- the
# integration proof that a real build succeeds through the extension lives in
# wp-scripts-extension-toolchain-no-local-smoke.sh and
# wp-scripts-extension-toolchain-custom-webpack-smoke.sh.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# shellcheck source=../lib/wp-scripts-toolchain.sh
source "${EXTENSION_DIR}/scripts/lib/wp-scripts-toolchain.sh"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-scripts-backward-compat.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# --- Fixture: a component with its own local wp-scripts -------------------
project_with_local="${TMP_DIR}/project-with-local"
mkdir -p "${project_with_local}/node_modules/.bin"
cat > "${project_with_local}/node_modules/.bin/wp-scripts" <<'SH'
#!/usr/bin/env bash
echo "LOCAL"
SH
chmod +x "${project_with_local}/node_modules/.bin/wp-scripts"

extension_dir="${TMP_DIR}/fake-extension"
toolchain_node_modules="${extension_dir}/toolchains/wp-scripts/node_modules"
mkdir -p "${toolchain_node_modules}/.bin" "${toolchain_node_modules}/@wordpress/scripts/config"
cat > "${toolchain_node_modules}/.bin/wp-scripts" <<'SH'
#!/usr/bin/env bash
echo "EXTENSION"
SH
chmod +x "${toolchain_node_modules}/.bin/wp-scripts"
cat > "${toolchain_node_modules}/@wordpress/scripts/config/webpack.config.js" <<'JS'
module.exports = {};
JS

original_path="$PATH"
unset NODE_PATH HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG || true

homeboy_wordpress_apply_extension_wp_scripts_toolchain "$project_with_local" "$extension_dir"

[ "$PATH" = "$original_path" ] || fail "PATH was modified for a component that already has a local wp-scripts binary"
[ -z "${NODE_PATH:-}" ] || fail "NODE_PATH was set for a component that already has a local wp-scripts binary"
[ -z "${HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG:-}" ] || fail "HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG was set for a component that already has a local wp-scripts binary"

echo "OK: local wp-scripts binary takes precedence, extension toolchain untouched"

# --- Fixture: a component with no local wp-scripts -------------------------
project_without_local="${TMP_DIR}/project-without-local"
mkdir -p "$project_without_local"

PATH="$original_path"
unset NODE_PATH HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG || true

homeboy_wordpress_apply_extension_wp_scripts_toolchain "$project_without_local" "$extension_dir"

case "$PATH" in
    "${toolchain_node_modules}/.bin:"*)
        ;;
    *)
        fail "PATH was not prefixed with the extension's toolchain node_modules/.bin for a component with no local wp-scripts"
        ;;
esac

case "${NODE_PATH:-}" in
    "${toolchain_node_modules}"*)
        ;;
    *)
        fail "NODE_PATH was not set to the extension's toolchain node_modules for a component with no local wp-scripts"
        ;;
esac

[ "${HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG:-}" = "${toolchain_node_modules}/@wordpress/scripts/config/webpack.config.js" ] \
    || fail "HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG did not resolve to the extension's default webpack config"

resolved_wp_scripts="$(command -v wp-scripts)"
output="$("$resolved_wp_scripts")"
[ "$output" = "EXTENSION" ] || fail "wp-scripts on PATH did not resolve to the extension's copy (got: $output)"

echo "OK: extension toolchain applied when no local wp-scripts binary exists"

# --- Fixture: neither the component nor the extension has wp-scripts -------
bare_extension_dir="${TMP_DIR}/bare-extension"
mkdir -p "$bare_extension_dir"

PATH="$original_path"
unset NODE_PATH HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG || true

homeboy_wordpress_apply_extension_wp_scripts_toolchain "$project_without_local" "$bare_extension_dir"

[ "$PATH" = "$original_path" ] || fail "PATH was modified even though the extension has no wp-scripts to offer"
[ -z "${NODE_PATH:-}" ] || fail "NODE_PATH was set even though the extension has no wp-scripts to offer"
[ -z "${HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG:-}" ] || fail "HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG was set even though the extension has no wp-scripts to offer"

echo "OK: no-op when neither the component nor the extension has wp-scripts"

# --- Detection helpers -------------------------------------------------------
declared_pkg="${TMP_DIR}/declared-package.json"
cat > "$declared_pkg" <<'JSON'
{
  "scripts": { "build": "wp-scripts build" },
  "devDependencies": { "@wordpress/scripts": "^30.15.0" }
}
JSON

script_only_pkg="${TMP_DIR}/script-only-package.json"
cat > "$script_only_pkg" <<'JSON'
{
  "scripts": { "build": "wp-scripts build" }
}
JSON

neither_pkg="${TMP_DIR}/neither-package.json"
cat > "$neither_pkg" <<'JSON'
{
  "scripts": { "build": "node build.js" }
}
JSON

homeboy_wordpress_package_declares_wp_scripts "$declared_pkg" \
    || fail "declared-dependency fixture should report @wordpress/scripts as declared"
homeboy_wordpress_package_scripts_invoke_wp_scripts "$declared_pkg" \
    || fail "declared-dependency fixture should report a wp-scripts-invoking script"

if homeboy_wordpress_package_declares_wp_scripts "$script_only_pkg"; then
    fail "script-only fixture must NOT report @wordpress/scripts as a declared dependency"
fi
homeboy_wordpress_package_scripts_invoke_wp_scripts "$script_only_pkg" \
    || fail "script-only fixture should report a wp-scripts-invoking script"

if homeboy_wordpress_package_declares_wp_scripts "$neither_pkg"; then
    fail "neither fixture must NOT report @wordpress/scripts as a declared dependency"
fi
if homeboy_wordpress_package_scripts_invoke_wp_scripts "$neither_pkg"; then
    fail "neither fixture must NOT report a wp-scripts-invoking script"
fi

echo "OK: declared-dependency vs. script-only vs. neither classification is correct"

echo "PASS: wp-scripts extension toolchain backward-compat contract"
