#!/usr/bin/env bash
set -euo pipefail

# Smoke for the wp-codebox test runner's component-path guard.
#
# Reproduces the failure mode where '--path .' resolves the component source to
# a shared scratch directory (or any directory whose only plugin header belongs
# to a foreign plugin). The Playground runtime detects the plugin main file by
# globbing "*.php" for a "Plugin Name:" header and taking the first alphabetical
# match, so a stray "an_main.php" can masquerade as the component and the wrong
# plugin gets mounted under the component slug — surfacing as a confusing
# load_component fatal. The guard must reject these cases loudly before mounting.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

RUNNER="${EXTENSION_PATH}/scripts/test/test-runner-wp-codebox.sh"

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

run_runner() {
    # $1 = component id, $2 = component path; captures combined output + status.
    local comp_id="$1"
    local comp_path="$2"
    local out="$3"
    set +e
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="$comp_id" \
    HOMEBOY_COMPONENT_PATH="$comp_path" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
        bash "$RUNNER" > "$out" 2>&1
    LAST_STATUS=$?
    set -e
}

# ---------------------------------------------------------------------------
# Case 1: a shared scratch directory holding a stray foreign plugin file must
# be rejected, not mounted.
# ---------------------------------------------------------------------------
scratch="${WORK}/scratch"
mkdir -p "${scratch}/tests"
cat > "${scratch}/an_main.php" <<'PHP'
<?php
/**
 * Plugin Name: Some Stray Plugin
 */
PHP

run_runner "extrachill-multisite" "$scratch" "${WORK}/case1.out"
if [ "$LAST_STATUS" -eq 0 ]; then
    echo "Case 1 FAILED: guard allowed a slug-mismatched directory to proceed (status 0)" >&2
    sed 's/^/  /' "${WORK}/case1.out" >&2
    exit 1
fi
assert_contains "${WORK}/case1.out" "none matches the expected slug 'extrachill-multisite'"
assert_contains "${WORK}/case1.out" "an_main.php"

# ---------------------------------------------------------------------------
# Case 2: an explicit shared temp root (e.g. /tmp) must be refused outright.
# ---------------------------------------------------------------------------
run_runner "extrachill-multisite" "/tmp" "${WORK}/case2.out"
if [ "$LAST_STATUS" -eq 0 ]; then
    echo "Case 2 FAILED: guard allowed /tmp as the component source (status 0)" >&2
    sed 's/^/  /' "${WORK}/case2.out" >&2
    exit 1
fi
assert_contains "${WORK}/case2.out" "shared temporary directory"

# ---------------------------------------------------------------------------
# Case 3: a directory whose plugin main file matches the slug passes the guard.
# We can't boot WP Codebox here, so we only assert the guard does NOT fire
# (i.e. it proceeds past validation into normal handling).
# ---------------------------------------------------------------------------
good="${WORK}/extrachill-multisite"
mkdir -p "${good}/tests"
cat > "${good}/extrachill-multisite.php" <<'PHP'
<?php
/**
 * Plugin Name: Extra Chill Multisite
 * Network: true
 */
PHP

run_runner "extrachill-multisite" "$good" "${WORK}/case3.out"
if grep -Fq "Refusing to mount a mismatched directory" "${WORK}/case3.out"; then
    echo "Case 3 FAILED: guard rejected a correctly-named component directory" >&2
    sed 's/^/  /' "${WORK}/case3.out" >&2
    exit 1
fi
if grep -Fq "shared temporary directory" "${WORK}/case3.out"; then
    echo "Case 3 FAILED: guard mis-flagged a real checkout as a scratch directory" >&2
    sed 's/^/  /' "${WORK}/case3.out" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Case 4: a fixture-style directory with tests/ but NO plugin header must be
# allowed past the guard (it routes to host-smoke / composer-script backends).
# ---------------------------------------------------------------------------
fixture="${WORK}/fixture"
mkdir -p "${fixture}/tests"
cat > "${fixture}/tests/example-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "fixture smoke ran\n" );
PHP

run_runner "fixture" "$fixture" "${WORK}/case4.out"
if grep -Fq "Refusing to mount a mismatched directory" "${WORK}/case4.out"; then
    echo "Case 4 FAILED: guard rejected a header-less fixture directory" >&2
    sed 's/^/  /' "${WORK}/case4.out" >&2
    exit 1
fi

echo "Component path guard smoke passed"
