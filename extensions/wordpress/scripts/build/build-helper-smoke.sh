#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-build-helper.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

isolated_extension="${TMP_DIR}/isolated-extension/wordpress"
mkdir -p "${isolated_extension}/scripts/lib"
cp "${EXTENSION_DIR}/scripts/lib/resolve-context.sh" "${isolated_extension}/scripts/lib/resolve-context.sh"
printf '{"id":"wordpress"}\n' > "${isolated_extension}/wordpress.json"

HOMEBOY_COMPONENT_PATH="${TMP_DIR}/component" \
SCRIPT_DIR="${isolated_extension}/scripts/lib" \
bash -c 'source "$1"; homeboy_resolve_context --component-alias PLUGIN_PATH; test "$PLUGIN_PATH" = "$HOMEBOY_COMPONENT_PATH"' \
    _ "${isolated_extension}/scripts/lib/resolve-context.sh"

component_dir="${TMP_DIR}/react-19-plugin"
fake_bin="${TMP_DIR}/bin"
npm_log="${TMP_DIR}/npm.log"
mkdir -p "$component_dir" "$fake_bin"

cat > "${fake_bin}/npm" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$NPM_LOG"
if [ "${1:-}" = "install" ] || [ "${1:-}" = "ci" ]; then
    mkdir -p node_modules/.bin
    : > node_modules/.bin/wp-scripts
fi
if [ "${1:-}" = "run" ]; then
    mkdir -p build
    : > build/index.js
fi
exit 0
SH
chmod +x "${fake_bin}/npm"

cat > "${component_dir}/react-19-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: React 19 Plugin
 * Version: 1.0.0
 */
PHP

cat > "${component_dir}/package.json" <<'JSON'
{
  "scripts": {
    "build": "wp-scripts build"
  },
  "devDependencies": {
    "@wordpress/scripts": "^32.0.0"
  }
}
JSON

(
    cd "$component_dir"
    PATH="${fake_bin}:$PATH" \
    NPM_LOG="$npm_log" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="react-19-plugin" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" >/dev/null
)

assert_contains "$npm_log" "install --silent --no-audit --no-fund --legacy-peer-deps"

echo "WordPress build helper smoke passed."
