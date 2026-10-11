#!/usr/bin/env bash
set -euo pipefail

# Standalone PHP smokes that load WordPress core source (for example
# wp-includes/html-api) without booting WordPress receive
# HOMEBOY_WORDPRESS_CORE_ROOT from `wp-codebox wordpress-source --json`.
# Resolution failure degrades to a warning: the smokes still run, without the
# variable, and resolution alone never fails the suite.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

component="${TMPDIR}/component"
core_root="${TMPDIR}/wordpress"
fake_wp_codebox="${TMPDIR}/bin/wp-codebox"
runtime_stub="${TMPDIR}/runtime.sh"
mkdir -p "${component}/tests" "${core_root}/wp-includes" "${TMPDIR}/bin" "${TMPDIR}/no-version-php"
printf '%s\n' '<?php' "\$wp_version = '7.1.2';" > "${core_root}/wp-includes/version.php"

# Each smoke asserts the contract it was given: the exact resolved root, or no
# variable at all (even when the parent environment carries a stale one).
for name in first second; do
    cat > "${component}/tests/core-root-${name}-smoke.php" <<'PHP'
<?php
$expected = getenv( 'EXPECTED_CORE_ROOT' );
$actual   = getenv( 'HOMEBOY_WORDPRESS_CORE_ROOT' );

if ( 'absent' === $expected ) {
    if ( false !== $actual ) {
        fwrite( STDERR, 'HOMEBOY_WORDPRESS_CORE_ROOT leaked: ' . var_export( $actual, true ) . "\n" );
        exit( 1 );
    }
    echo "standalone smoke ran without core root\n";
    exit( 0 );
}
if ( $actual !== $expected || ! is_file( $actual . '/wp-includes/version.php' ) ) {
    fwrite( STDERR, 'HOMEBOY_WORDPRESS_CORE_ROOT was ' . var_export( $actual, true ) . "\n" );
    exit( 1 );
}
echo "standalone smoke received core root\n";
PHP
done

cat > "${component}/homeboy-test-manifest.json" <<'JSON'
{
    "schema": "homeboy/test-manifest/v1",
    "tests": {
        "tests/core-root-first-smoke.php": {"environment": "standalone-php"},
        "tests/core-root-second-smoke.php": {"environment": "standalone-php"}
    }
}
JSON

cat > "$runtime_stub" <<'SH'
#!/usr/bin/env bash
echo "PHPUNIT_RUNTIME_INVOKED"
SH
chmod +x "$runtime_stub"

# Fake WP Codebox CLI. FAKE_MODE selects the wordpress-source behavior; every
# wordpress-source invocation is appended to FAKE_LOG.
cat > "$fake_wp_codebox" <<'SH'
#!/usr/bin/env bash
case "${1:-}" in
    commands)
        echo "recipe-run"
        [ "$FAKE_MODE" = "too-old" ] || echo "wordpress-source"
        exit 0
        ;;
    wordpress-source)
        printf '%s\n' "$*" >> "$FAKE_LOG"
        case "$FAKE_MODE" in
            ok)
                printf '{"schema":"wp-codebox/wordpress-source/v1","version":"7.1.2","root":"%s","archive":"%s.zip","sourceUrl":"https://wordpress.org/wordpress-7.1.2.zip"}\n' "$FAKE_ROOT" "$FAKE_ROOT"
                exit 0
                ;;
            offline)
                echo "Error: could not download WordPress: getaddrinfo ENOTFOUND wordpress.org" >&2
                exit 1
                ;;
            too-old)
                echo "error: unknown command 'wordpress-source'" >&2
                exit 1
                ;;
            bad-schema)
                printf '{"schema":"wp-codebox/other/v1","root":"%s"}\n' "$FAKE_ROOT"
                exit 0
                ;;
            not-json)
                echo "Downloading WordPress..."
                exit 0
                ;;
        esac
        ;;
esac
echo "unexpected fake wp-codebox invocation: $*" >&2
exit 64
SH
chmod +x "$fake_wp_codebox"

run_case() {
    local mode="$1"
    local root="$2"
    local expected="$3"
    local settings="$4"
    local bin="${5:-$fake_wp_codebox}"
    local invocations="${6:-1}"
    local status

    : > "${TMPDIR}/${mode}.log"
    set +e
    FAKE_MODE="$mode" \
    FAKE_ROOT="$root" \
    FAKE_LOG="${TMPDIR}/${mode}.log" \
    EXPECTED_CORE_ROOT="$expected" \
    HOMEBOY_WORDPRESS_CORE_ROOT="/stale/inherited/root" \
    HOMEBOY_WP_CODEBOX_BIN="$bin" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="$runtime_stub" \
    HOMEBOY_SETTINGS_JSON="$settings" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/${mode}.out" 2> "${TMPDIR}/${mode}.err"
    status=$?
    set -e

    if [ "$status" -ne 0 ]; then
        echo "FAIL: ${mode}: test runner exited ${status}" >&2
        cat "${TMPDIR}/${mode}.out" "${TMPDIR}/${mode}.err" >&2
        exit 1
    fi
    grep -Fq "PHP_SMOKE_SUMMARY:passed=2 failed=0" "${TMPDIR}/${mode}.out" || {
        echo "FAIL: ${mode}: standalone smokes did not both pass" >&2
        cat "${TMPDIR}/${mode}.out" "${TMPDIR}/${mode}.err" >&2
        exit 1
    }
    # Resolved once per run, not once per smoke file.
    [ "$(wc -l < "${TMPDIR}/${mode}.log" | tr -d ' ')" = "$invocations" ] || {
        echo "FAIL: ${mode}: expected ${invocations} wordpress-source invocation(s)" >&2
        cat "${TMPDIR}/${mode}.log" >&2
        exit 1
    }
}

assert_degraded() {
    local mode="$1"
    local reason="$2"
    local warnings

    grep -Fq "standalone smoke ran without core root" "${TMPDIR}/${mode}.out"
    if grep -Fq "WordPress core:" "${TMPDIR}/${mode}.out"; then
        echo "FAIL: ${mode}: reported a WordPress core root" >&2
        exit 1
    fi
    warnings="$(grep -c '^WARNING: WordPress core source unavailable for standalone smokes: ' "${TMPDIR}/${mode}.err" || true)"
    [ "$warnings" = "1" ] || {
        echo "FAIL: ${mode}: expected one warning line, saw ${warnings}" >&2
        cat "${TMPDIR}/${mode}.err" >&2
        exit 1
    }
    grep -Fq "$reason" "${TMPDIR}/${mode}.err" || {
        echo "FAIL: ${mode}: warning did not explain '${reason}'" >&2
        cat "${TMPDIR}/${mode}.err" >&2
        exit 1
    }
}

# (a) Valid output: the smokes see the resolved root at the configured version.
run_case ok "$core_root" "$core_root" '{"wordpress_runtime_version":"6.9"}'
grep -Fq "standalone smoke received core root" "${TMPDIR}/ok.out"
grep -Fq "  WordPress core: ${core_root} (7.1.2)" "${TMPDIR}/ok.out"
grep -Fxq "wordpress-source --wp 6.9 --json" "${TMPDIR}/ok.log"
if grep -Fq "WARNING:" "${TMPDIR}/ok.err"; then
    echo "FAIL: ok: unexpected warning" >&2
    cat "${TMPDIR}/ok.err" >&2
    exit 1
fi

# The legacy setting name feeds the same query; an unset version omits --wp.
run_case ok "$core_root" "$core_root" '{"wp_codebox_wordpress_version":"6.8"}'
grep -Fxq "wordpress-source --wp 6.8 --json" "${TMPDIR}/ok.log"
run_case ok "$core_root" "$core_root" '{}'
grep -Fxq "wordpress-source --json" "${TMPDIR}/ok.log"

# (b) Failures degrade to one warning; smokes run without the variable.
run_case offline "$core_root" absent '{}'
assert_degraded offline "ENOTFOUND wordpress.org"

run_case too-old "$core_root" absent '{}'
assert_degraded too-old "unknown command 'wordpress-source'"

run_case bad-schema "$core_root" absent '{}'
assert_degraded bad-schema "schema wp-codebox/wordpress-source/v1"

run_case not-json "$core_root" absent '{}'
assert_degraded not-json "schema wp-codebox/wordpress-source/v1"

run_case ok "${TMPDIR}/no-version-php" absent '{}'
assert_degraded ok "has no wp-includes/version.php"

run_case missing "$core_root" absent '{}' "${TMPDIR}/bin/missing-wp-codebox" 0
assert_degraded missing "configured WP Codebox binary is unavailable"

echo "Standalone WordPress core root smoke passed"
