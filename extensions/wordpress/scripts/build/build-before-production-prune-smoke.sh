#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
RESOLVE_CONTEXT_CORE_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/crates/homeboy-core/src/extension/runtime/resolve-context.sh}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-build-order.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

fake_bin="${TMP_DIR}/bin"
component_dir="${TMP_DIR}/fixture-plugin"
phase_log="${TMP_DIR}/phases.log"
mkdir -p "$fake_bin" "${component_dir}/includes"

cat > "${fake_bin}/npm" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "ls" ]; then
    exit 0
fi

if [ "${1:-}" = "run" ] && [ "${2:-}" = "build" ]; then
    printf '%s\n' frontend-build >> "$PHASE_LOG"
    [ "${NPM_FAIL_BUILD:-0}" != "1" ] || {
        echo "required frontend build failed" >&2
        exit 1
    }
    [ -f node_modules/fixture-builder/node_modules/build-transitive/index.js ] || {
        echo "missing dev-only transitive build dependency" >&2
        exit 1
    }
    mkdir -p build
    printf '%s\n' 'compiled with dev-only transitive dependency' > build/index.js
fi
SH
chmod +x "${fake_bin}/npm"

cat > "${fake_bin}/composer" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = "install" ] || exit 0

if printf '%s\n' "$*" | grep -Fq -- '--no-dev'; then
    printf '%s\n' production-prune >> "$PHASE_LOG"
    rm -rf node_modules/fixture-builder
    mkdir -p vendor
    printf '%s\n' '<?php // production runtime dependency' > vendor/autoload.php
else
    mkdir -p node_modules/fixture-builder/node_modules/build-transitive
    printf '%s\n' 'module.exports = true;' > node_modules/fixture-builder/node_modules/build-transitive/index.js
fi
SH
chmod +x "${fake_bin}/composer"

cat > "${component_dir}/fixture-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Frontend Build Order Fixture
 * Version: 1.0.0
 */
PHP
printf '%s\n' '<?php' > "${component_dir}/includes/bootstrap.php"

cat > "${component_dir}/composer.json" <<'JSON'
{
  "name": "fixture/frontend-build-order",
  "require": {
    "fixture/runtime-package": "1.0.0"
  },
  "require-dev": {
    "fixture/test-package": "1.0.0"
  }
}
JSON

cat > "${component_dir}/package.json" <<'JSON'
{
  "scripts": {
    "build": "wp-scripts build"
  },
  "devDependencies": {
    "@wordpress/scripts": "1.0.0",
    "fixture-builder": "1.0.0"
  }
}
JSON

printf '%s\n' '{"lockfileVersion":3}' > "${component_dir}/package-lock.json"
mkdir -p "${component_dir}/node_modules/.bin" \
    "${component_dir}/node_modules/fixture-builder/node_modules/build-transitive"
: > "${component_dir}/node_modules/.bin/wp-scripts"
: > "${component_dir}/node_modules/.package-lock.json"
printf '%s\n' 'module.exports = true;' \
    > "${component_dir}/node_modules/fixture-builder/node_modules/build-transitive/index.js"

(
    cd "$component_dir"
    PATH="${fake_bin}:$PATH" \
    PHASE_LOG="$phase_log" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="fixture-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_REQUIRE_FRONTEND=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/build.out" 2>&1
) || {
    sed 's/^/  /' "${TMP_DIR}/build.out" >&2
    fail "package build failed"
}

expected_phases="$(printf 'frontend-build\nproduction-prune')"
[ "$(cat "$phase_log")" = "$expected_phases" ] \
    || fail "frontend build did not run before production pruning"

zip_file="${component_dir}/build/fixture-plugin.zip"
[ -f "$zip_file" ] || fail "production ZIP was not created"
unzip -Z1 "$zip_file" | grep -Fq 'fixture-plugin/build/index.js' \
    || fail "compiled frontend asset is missing from production ZIP"
unzip -Z1 "$zip_file" | grep -Fq 'fixture-plugin/vendor/autoload.php' \
    || fail "production runtime dependency is missing from ZIP"
if unzip -Z1 "$zip_file" | grep -Fq 'node_modules'; then
    fail "development dependencies leaked into production ZIP"
fi

rm -f "$zip_file"
: > "$phase_log"
if (
    cd "$component_dir"
    PATH="${fake_bin}:$PATH" \
    PHASE_LOG="$phase_log" \
    NPM_FAIL_BUILD=1 \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="fixture-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_REQUIRE_FRONTEND=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "${TMP_DIR}/failed-build.out" 2>&1
); then
    fail "required frontend build failure was accepted"
fi

[ ! -f "$zip_file" ] || fail "frontend build failure produced a nominal ZIP"
[ "$(cat "$phase_log")" = "frontend-build" ] \
    || fail "production pruning ran after a required frontend build failure"
grep -Fq 'required frontend build failed' "${TMP_DIR}/failed-build.out" \
    || fail "frontend build diagnostic was not preserved"

echo "WordPress frontend build order smoke passed."
