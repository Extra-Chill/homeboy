#!/usr/bin/env bash
set -euo pipefail

# Native test runner for wordpress-develop / WordPress core source checkouts.
# This runner intentionally does not use Playground: core-dev checkouts are the
# ecosystem itself and rely on WordPress core's own PHPUnit bootstrap.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=../lib/resolve-context.sh
source "${RESOLVE_CONTEXT_HELPER}"
homeboy_resolve_context

CORE_PATH="$PLUGIN_PATH"

fail() {
    echo "Error: $*" >&2
    exit 1
}

is_core_dev_checkout() {
    [ -f "${CORE_PATH}/wp-config-sample.php" ] \
        && [ -f "${CORE_PATH}/src/wp-includes/version.php" ] \
        && [ -d "${CORE_PATH}/tests/phpunit" ]
}

ensure_core_dev_checkout() {
    if ! is_core_dev_checkout; then
        fail "core-dev runner expected wordpress-develop markers: wp-config-sample.php, src/wp-includes/version.php, tests/phpunit/"
    fi
}

newer_than_build() {
    [ -d "${CORE_PATH}/build" ] || return 0
    find "${CORE_PATH}/src" -type f -newer "${CORE_PATH}/build" 2>/dev/null | grep -q .
}

ensure_dependencies() {
    cd "$CORE_PATH"

    if [ ! -d node_modules ]; then
        command -v npm >/dev/null 2>&1 || fail "npm is required to install wordpress-develop dependencies"
        echo "Installing npm dependencies for wordpress-develop..."
        npm install
    fi

    if [ ! -d vendor ]; then
        command -v composer >/dev/null 2>&1 || fail "composer is required to install wordpress-develop PHP dependencies"
        echo "Installing Composer dependencies for wordpress-develop..."
        composer install --no-interaction
    fi

    if [ ! -d build ] || newer_than_build; then
        command -v npm >/dev/null 2>&1 || fail "npm is required to build wordpress-develop"
        echo "Building wordpress-develop..."
        npm run build
    fi
}

write_wp_tests_config_from_env() {
    local config="${CORE_PATH}/wp-tests-config.php"
    [ -f "$config" ] && return 0

    local sample="${CORE_PATH}/wp-tests-config-sample.php"
    [ -f "$sample" ] || sample="${CORE_PATH}/wp-config-sample.php"
    [ -f "$sample" ] || fail "wp-tests-config.php missing and no sample config found"

    local db_name="${HOMEBOY_WP_TESTS_DB_NAME:-${WP_TESTS_DB_NAME:-}}"
    local db_user="${HOMEBOY_WP_TESTS_DB_USER:-${WP_TESTS_DB_USER:-}}"
    local db_pass="${HOMEBOY_WP_TESTS_DB_PASSWORD:-${WP_TESTS_DB_PASSWORD:-}}"
    local db_host="${HOMEBOY_WP_TESTS_DB_HOST:-${WP_TESTS_DB_HOST:-localhost}}"

    if [ -z "$db_name" ] || [ -z "$db_user" ]; then
        cat >&2 <<'EOF'
Error: wp-tests-config.php is missing.

Create it in the wordpress-develop checkout, or set:
  HOMEBOY_WP_TESTS_DB_NAME
  HOMEBOY_WP_TESTS_DB_USER
  HOMEBOY_WP_TESTS_DB_PASSWORD
  HOMEBOY_WP_TESTS_DB_HOST (optional, defaults to localhost)
EOF
        exit 1
    fi

    php -r '
        $sample = file_get_contents($argv[1]);
        $replacements = [
            "youremptytestdbnamehere" => $argv[3],
            "yourusernamehere" => $argv[4],
            "yourpasswordhere" => $argv[5],
            "localhost" => $argv[6],
        ];
        file_put_contents($argv[2], strtr($sample, $replacements));
    ' "$sample" "$config" "$db_name" "$db_user" "$db_pass" "$db_host"
}

ensure_core_dev_checkout

if [ "${HOMEBOY_CORE_DEV_DRY_RUN:-}" = "1" ]; then
    echo "core-dev test runner selected: ${CORE_PATH}"
    exit 0
fi

ensure_dependencies
write_wp_tests_config_from_env

cd "$CORE_PATH"

PHPUNIT_BIN="${CORE_PATH}/vendor/bin/phpunit"
[ -x "$PHPUNIT_BIN" ] || fail "PHPUnit not found at ${PHPUNIT_BIN}; run composer install"

PHPUNIT_CONFIG="tests/phpunit/phpunit.xml.dist"
if [ ! -f "$PHPUNIT_CONFIG" ]; then
    PHPUNIT_CONFIG="phpunit.xml.dist"
fi
[ -f "$PHPUNIT_CONFIG" ] || fail "No WordPress core PHPUnit config found"

PHPUNIT_ARGS=(-c "$PHPUNIT_CONFIG")
if [ "${HOMEBOY_WORDPRESS_MULTISITE:-}" = "1" ] && [ -f "tests/phpunit/multisite.xml" ]; then
    PHPUNIT_ARGS=(-c "tests/phpunit/multisite.xml")
fi
if [ -n "${HOMEBOY_TEST_FILTER:-}" ]; then
    PHPUNIT_ARGS+=(--filter "${HOMEBOY_TEST_FILTER}")
fi
if [ -n "${HOMEBOY_TEST_GROUP:-}" ]; then
    PHPUNIT_ARGS+=(--group "${HOMEBOY_TEST_GROUP}")
fi
PHPUNIT_ARGS+=("$@")

echo "Running WordPress core PHPUnit tests..."
set +e
PHPUNIT_OUTPUT=$("$PHPUNIT_BIN" "${PHPUNIT_ARGS[@]}" 2>&1)
PHPUNIT_EXIT=$?
set -e

printf '%s\n' "$PHPUNIT_OUTPUT"

PARSE_RESULTS="${EXTENSION_PATH}/scripts/test/parse-test-results.sh"
PARSE_FAILURES="${EXTENSION_PATH}/scripts/test/parse-test-failures.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
    printf '%s\n' "$PHPUNIT_OUTPUT" | bash "$PARSE_RESULTS" || true
fi
if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ] && [ -f "$PARSE_FAILURES" ]; then
    printf '%s\n' "$PHPUNIT_OUTPUT" | bash "$PARSE_FAILURES" "$CORE_PATH" || true
fi

exit "$PHPUNIT_EXIT"
