#!/usr/bin/env bash
set -euo pipefail

# Regression: Extra-Chill/homeboy#12023 — "changed-test scope selects tests then
# executes zero".
#
# The changed-file router classified selected paths into JS smoke, shell smoke,
# Node test and PHPUnit buckets. Anything else fell into a terminal `else` that
# recorded only "something non-host is in scope" and then discarded the path.
# Standalone `tests/**/*-smoke.php` scripts match none of those four shapes, so
# every one of them was dropped without ever reaching a runner — even though the
# runners for them already existed and were already wired up for the
# exclusive_env scope kind.
#
# In the reported run a 92-file changed scope routed 6 PHPUnit classes and
# silently discarded the other 86, then reported 0 executed tests.
#
# These scenarios assert that every selected path is either routed to a named
# runner or excluded with a recorded reason, and that the counts reconcile.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

component="${WORKDIR}/component"
mkdir -p "${component}/tests/Unit/Support" "${WORKDIR}/stubs"

runner_prelude="${WORKDIR}/runner-prelude.sh"
cat > "$runner_prelude" <<'SH'
homeboy_runner_init() {
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
}
SH

cat > "${component}/tests/standalone-smoke.php" <<'PHP'
<?php
echo "standalone smoke ran\n";
PHP

cat > "${component}/tests/wordpress-smoke.php" <<'PHP'
<?php
echo "wordpress smoke ran\n";
PHP

cat > "${component}/tests/failing-smoke.php" <<'PHP'
<?php
fwrite(STDERR, "failing smoke\n");
exit(3);
PHP

cat > "${component}/tests/Unit/OwnershipTest.php" <<'PHP'
<?php
class OwnershipTest extends PHPUnit\Framework\TestCase {}
PHP

# A test double is legitimately not an executable test. Excluding it is correct;
# excluding it silently is not.
cat > "${component}/tests/Unit/Support/TestDoubles.php" <<'PHP'
<?php
// Shared test double. Not a runnable test case.
PHP

cat > "${component}/homeboy-test-manifest.json" <<'JSON'
{
  "schema": "homeboy/test-manifest/v1",
  "tests": {
    "tests/standalone-smoke.php": {
      "environment": "standalone-php"
    },
    "tests/failing-smoke.php": {
      "environment": "standalone-php"
    },
    "tests/wordpress-smoke.php": {
      "environment": "wordpress"
    }
  }
}
JSON

# Stand in for the booted-WordPress host smoke runner.
cat > "${WORKDIR}/stubs/host-smoke-wp.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
passed=0
while IFS= read -r f; do
    [ -n "$f" ] || continue
    echo "HOST_SMOKE_BEGIN:${f}"
    echo "HOST_SMOKE_OK:${f}"
    passed=$((passed + 1))
done <<< "${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES:-${HOMEBOY_WORDPRESS_HOST_SMOKE_FILE:-}}"
echo "HOST_SMOKE_SUMMARY:passed=${passed} failed=0"
SH
chmod +x "${WORKDIR}/stubs/host-smoke-wp.sh"

# Stand in for the WordPress runtime (PHPUnit) backend. Reporting the scope it
# received is what proves the PHPUnit selection was not widened to the suite.
cat > "${WORKDIR}/stubs/wp-codebox.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "WP_CODEBOX_STUB"
printf 'PHPUNIT_CHANGED=%s\n' "${HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES:-}"
SH
chmod +x "${WORKDIR}/stubs/wp-codebox.sh"

run_changed_scope() {
    local outfile="$1"
    local changed="$2"
    local status=0
    set +e
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${WORKDIR}/stubs/host-smoke-wp.sh" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_CHANGED_TEST_FILES="$changed" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "$outfile" 2>&1
    status=$?
    set -e
    return "$status"
}

# --- Scenario 1: the acceptance criterion from the issue --------------------
# A changed scope containing a PHPUnit class plus standalone smoke scripts, plus
# a non-test helper. Every path must be accounted for.
mixed="${WORKDIR}/mixed.out"
run_changed_scope "$mixed" $'tests/Unit/OwnershipTest.php\ntests/standalone-smoke.php\ntests/wordpress-smoke.php\ntests/Unit/Support/TestDoubles.php'

assert_contains "$mixed" "CHANGED_SCOPE_ROUTE:tests/Unit/OwnershipTest.php:runner=phpunit"
assert_contains "$mixed" "CHANGED_SCOPE_ROUTE:tests/standalone-smoke.php:runner=host-php-smoke"
assert_contains "$mixed" "CHANGED_SCOPE_ROUTE:tests/wordpress-smoke.php:runner=host-php-smoke"
assert_contains "$mixed" "CHANGED_SCOPE_EXCLUDED:tests/Unit/Support/TestDoubles.php:reason=unsupported_test_shape"
assert_contains "$mixed" "CHANGED_SCOPE_SUMMARY:selected=4 routed=3 excluded=1"

# The standalone smoke actually executed...
assert_contains "$mixed" "PHP_SMOKE_BEGIN:tests/standalone-smoke.php"
assert_contains "$mixed" "PHP_SMOKE_OK:tests/standalone-smoke.php"
# ...the manifest-declared WordPress smoke went to the booted-WordPress runner...
assert_contains "$mixed" "HOST_SMOKE_BEGIN:tests/wordpress-smoke.php"
# ...and the PHPUnit class still reached the backend as an explicit scope rather
# than an empty one, which WP Codebox reads as "run everything".
assert_contains "$mixed" "WP_CODEBOX_STUB"
assert_contains "$mixed" "PHPUNIT_CHANGED=tests/Unit/OwnershipTest.php"

# --- Scenario 2: PHP smokes alone are a complete scope ----------------------
# Nothing here needs the PHPUnit backend. Falling through to it would hand WP
# Codebox an empty changed-test list and silently widen to the full suite.
only_smokes="${WORKDIR}/only-smokes.out"
run_changed_scope "$only_smokes" $'tests/standalone-smoke.php\ntests/wordpress-smoke.php'

assert_contains "$only_smokes" "PHP_SMOKE_OK:tests/standalone-smoke.php"
assert_contains "$only_smokes" "HOST_SMOKE_BEGIN:tests/wordpress-smoke.php"
assert_contains "$only_smokes" "CHANGED_SCOPE_SUMMARY:selected=2 routed=2 excluded=0"
assert_not_contains "$only_smokes" "WP_CODEBOX_STUB"

# --- Scenario 3: a failing smoke fails the scope ----------------------------
# The runner execs the PHPUnit backend, which would replace the process and
# report only the backend's status. A passing backend must not erase a failing
# host smoke.
failing="${WORKDIR}/failing.out"
failing_status=0
run_changed_scope "$failing" $'tests/Unit/OwnershipTest.php\ntests/failing-smoke.php' || failing_status=$?

assert_contains "$failing" "PHP_SMOKE_FAIL:tests/failing-smoke.php:exit=3"
assert_contains "$failing" "WP_CODEBOX_STUB"
if [ "$failing_status" -eq 0 ]; then
    echo "Expected a failing host PHP smoke to fail the changed scope, got exit 0" >&2
    sed 's/^/  /' "$failing" >&2
    exit 1
fi

# --- Scenario 4: --file on a standalone smoke stops there -------------------
# The WordPress-environment branch execs and therefore terminates; the
# standalone branch returns, and without an explicit exit a request for one file
# fell through into the full-suite PHPUnit run below it.
single="${WORKDIR}/single-file.out"
single_status=0
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${WORKDIR}/stubs/host-smoke-wp.sh" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/standalone-smoke.php > "$single" 2>&1
single_status=$?
set -e

if [ "$single_status" -ne 0 ]; then
    echo "Expected --file on a standalone smoke to succeed, got exit ${single_status}" >&2
    sed 's/^/  /' "$single" >&2
    exit 1
fi
assert_contains "$single" "PHP_SMOKE_OK:tests/standalone-smoke.php"
assert_not_contains "$single" "WP_CODEBOX_STUB"

echo "Changed-scope test routing smoke passed"
