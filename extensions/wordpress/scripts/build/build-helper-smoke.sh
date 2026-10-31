#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
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
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" >/dev/null
)

assert_contains "$npm_log" "install --no-audit --no-fund --legacy-peer-deps"

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        echo "Expected $file to NOT contain: $unexpected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

# Lockfile-aware install command selection.
#
# A committed package-lock.json is authoritative → `npm ci` (strict).
# A gitignored/untracked package-lock.json is a local artifact → `npm install`
# (refresh), because nothing keeps an ignored lockfile in sync with
# package.json and `npm ci` would fail on an unfixable desync.
make_lockfile_component() {
    local dir="$1"
    mkdir -p "$dir"
    cat > "${dir}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Lockfile Plugin
 * Version: 1.0.0
 */
PHP
    cat > "${dir}/package.json" <<'JSON'
{
  "scripts": {
    "build": "wp-scripts build"
  },
  "devDependencies": {
    "@wordpress/scripts": "^30.0.0"
  }
}
JSON
    printf '{"lockfileVersion":3}\n' > "${dir}/package-lock.json"
}

run_build() {
    local dir="$1"
    local log="$2"
    (
        cd "$dir"
        PATH="${fake_bin}:$PATH" \
        NPM_LOG="$log" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        HOMEBOY_COMPONENT_ID="lockfile-plugin" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
        HOMEBOY_SKIP_TESTS=1 \
            bash "${EXTENSION_DIR}/scripts/build/build.sh" >/dev/null
    )
}

# Case 1: committed lockfile → npm ci
committed_dir="${TMP_DIR}/committed-lockfile"
committed_log="${TMP_DIR}/committed-npm.log"
make_lockfile_component "$committed_dir"
(
    cd "$committed_dir"
    git init -q
    git config user.email smoke@example.com
    git config user.name smoke
    git add -A
    git commit -qm "initial"
)
run_build "$committed_dir" "$committed_log"
assert_contains "$committed_log" "ci --no-audit --no-fund"
assert_not_contains "$committed_log" "install --no-audit --no-fund"

# Case 2: gitignored lockfile → npm install (refresh), never npm ci
ignored_dir="${TMP_DIR}/ignored-lockfile"
ignored_log="${TMP_DIR}/ignored-npm.log"
make_lockfile_component "$ignored_dir"
printf 'package-lock.json\nnode_modules/\n' > "${ignored_dir}/.gitignore"
(
    cd "$ignored_dir"
    git init -q
    git config user.email smoke@example.com
    git config user.name smoke
    git add -A
    git commit -qm "initial"
)
run_build "$ignored_dir" "$ignored_log"
assert_contains "$ignored_log" "install --no-audit --no-fund"
assert_not_contains "$ignored_log" "ci --no-audit --no-fund"

# Case 3: vendored dependency fixtures are ignored by nested frontend builds.
vendor_fixture_dir="${TMP_DIR}/vendor-fixture"
vendor_fixture_log="${TMP_DIR}/vendor-fixture-npm.log"
make_lockfile_component "$vendor_fixture_dir"
mkdir -p "${vendor_fixture_dir}/vendor/example/fixture"
cat > "${vendor_fixture_dir}/vendor/example/fixture/package.json" <<'JSON'
{
  "scripts": {
    "build": "node should-not-run.js"
  }
}
JSON
run_build "$vendor_fixture_dir" "$vendor_fixture_log"
assert_not_contains "$vendor_fixture_log" "should-not-run.js"

echo "WordPress build helper smoke passed."
