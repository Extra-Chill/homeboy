#!/usr/bin/env bash
#
# Browser-target handoff smoke test.
#
# Exercises the WordPress bench browser-target helper directly. This keeps the
# smoke focused on the shared-state handoff shape without booting Playground or
# pretending the PHP bench runner owns Playwright/browser lifecycle.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=browser-target.sh
source "${SCRIPT_DIR}/browser-target.sh"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for JSON assertions in this smoke." >&2
    exit 1
fi

SHARED_STATE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-browser-target-smoke.XXXXXX")
cleanup() {
    rm -rf "$SHARED_STATE_DIR"
}
trap cleanup EXIT

export BROWSER_TARGET_PASSWORD="super-secret-smoke-password"

SETTINGS_JSON=$(cat <<'JSON'
{
  "bench_browser_target": {
    "enabled": true,
    "baseUrl": "http://127.0.0.1:8888/",
    "login": {
      "method": "credentials",
      "username": "admin",
      "password_env": "BROWSER_TARGET_PASSWORD"
    }
  }
}
JSON
)

echo "============================================"
echo "Playground bench browser-target smoke test"
echo "============================================"
echo "Shared state: $SHARED_STATE_DIR"
echo ""

helper_output=$(homeboy_wordpress_emit_browser_target "$SETTINGS_JSON" "$SHARED_STATE_DIR" "browser-fixture" "browser-fixture" "installed")
TARGET_FILE="${SHARED_STATE_DIR}/browser-target.json"

if [ ! -s "$TARGET_FILE" ]; then
    echo "ERROR: browser-target.json missing or empty" >&2
    exit 1
fi

if [[ "$helper_output" == *"$BROWSER_TARGET_PASSWORD"* ]]; then
    echo "ERROR: helper output leaked the browser target password" >&2
    exit 1
fi

assert_json() {
    local filter="$1"
    local expected="$2"
    local actual
    actual=$(jq -r "$filter" "$TARGET_FILE")
    if [ "$actual" != "$expected" ]; then
        echo "ERROR: expected $filter => $expected, got $actual" >&2
        echo "--- browser-target.json ---" >&2
        jq . "$TARGET_FILE" >&2
        exit 1
    fi
}

assert_json '.schemaVersion' '1'
assert_json '.kind' 'wordpress'
assert_json '.baseUrl' 'http://127.0.0.1:8888/'
assert_json '.adminUrl' 'http://127.0.0.1:8888/wp-admin/'
assert_json '.lifecycle.server' 'external'
assert_json '.lifecycle.keepAlive' 'caller'
assert_json '.login.method' 'credentials'
assert_json '.login.username' 'admin'
assert_json '.login.password' "$BROWSER_TARGET_PASSWORD"
assert_json '.metadata.componentId' 'browser-fixture'
assert_json '.metadata.pluginSlug' 'browser-fixture'
assert_json '.metadata.benchSiteMode' 'installed'
assert_json '.artifactPolicy.publishRaw' 'false'

if jq -e 'has("password_env") or has("passwordEnv") or (.login | has("password_env") or has("passwordEnv"))' "$TARGET_FILE" >/dev/null; then
    echo "ERROR: password env indirection leaked into browser-target.json" >&2
    exit 1
fi

echo "--- browser-target.json ---"
jq '(.login.password) = "<redacted>"' "$TARGET_FILE"
echo ""
echo "============================================"
echo "✓ Browser-target handoff smoke test PASSED"
echo "============================================"
