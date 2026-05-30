#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
FIXTURE="${EXTENSION_PATH}/tests/fixtures/wordpress-develop-core-dev"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_equals() {
    local actual="$1"
    local expected="$2"
    local label="$3"
    if [ "$actual" != "$expected" ]; then
        echo "Expected $label to be '$expected', got '$actual'" >&2
        exit 1
    fi
}

prepare_core_lint_fixture() {
    local target="$1"

    cp -R "$FIXTURE" "$target"
    mkdir -p "${target}/vendor/bin"
    cat > "${target}/vendor/bin/phpcs" <<'EOF'
#!/usr/bin/env bash
exit "${HOMEBOY_FAKE_PHPCS_EXIT:-0}"
EOF
    cat > "${target}/vendor/bin/phpstan" <<'EOF'
#!/usr/bin/env bash
exit "${HOMEBOY_FAKE_PHPSTAN_EXIT:-0}"
EOF
    chmod +x "${target}/vendor/bin/phpcs" "${target}/vendor/bin/phpstan"

    git -C "$target" init -q
    git -C "$target" config user.email smoke@example.com
    git -C "$target" config user.name 'Smoke Test'
    git -C "$target" add .
    git -C "$target" commit -q -m 'base'
    printf '\n// changed\n' >> "${target}/src/wp-includes/version.php"
}

run_core_lint_fixture() {
    local target="$1"
    shift

    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="wordpress-develop" \
    HOMEBOY_COMPONENT_PATH="$target" \
    HOMEBOY_COMPONENT_SHAPE="core-dev" \
    HOMEBOY_CHANGED_SINCE="HEAD" \
        "$@" bash "${EXTENSION_PATH}/scripts/lint/lint-runner.sh"
}

source "${EXTENSION_PATH}/scripts/lib/detect-component.sh"
homeboy_detect_component "$FIXTURE"
assert_equals "$HOMEBOY_COMPONENT_TYPE" "core-dev" "component type"
assert_equals "$HOMEBOY_COMPONENT_NAME" "WordPress Core Development" "component name"
assert_contains <(printf '%s\n' "$HOMEBOY_COMPONENT_MAIN_FILE") "src/wp-includes/version.php"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="wordpress-develop" \
HOMEBOY_COMPONENT_PATH="$FIXTURE" \
HOMEBOY_COMPONENT_SHAPE="core-dev" \
HOMEBOY_CORE_DEV_DRY_RUN=1 \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/test-explicit.out"
assert_contains "${TMPDIR}/test-explicit.out" "core-dev WP Codebox test runner selected"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="wordpress-develop" \
HOMEBOY_COMPONENT_PATH="$FIXTURE" \
HOMEBOY_CORE_DEV_DRY_RUN=1 \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/test-detected.out"
assert_contains "${TMPDIR}/test-detected.out" "core-dev WP Codebox test runner selected"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="wordpress-develop" \
HOMEBOY_COMPONENT_PATH="$FIXTURE" \
HOMEBOY_COMPONENT_SHAPE="core-dev" \
HOMEBOY_WORDPRESS_TEST_BACKEND="core-native" \
HOMEBOY_CORE_DEV_DRY_RUN=1 \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/test-native.out" 2>&1 && {
        echo "Expected core-native backend to be rejected" >&2
        exit 1
    }
assert_contains "${TMPDIR}/test-native.out" "Supported core-dev backend: wp-codebox"

CORE_WP_CODEBOX_FIXTURE="${TMPDIR}/core-wp-codebox-fixture"
cp -R "$FIXTURE" "$CORE_WP_CODEBOX_FIXTURE"
mkdir -p "${CORE_WP_CODEBOX_FIXTURE}/tests/phpunit/tests"
cat > "${CORE_WP_CODEBOX_FIXTURE}/tests/phpunit/tests/basic.php" <<'PHP'
<?php
final class Basic_Core_Smoke_Test extends WP_UnitTestCase {
    public function test_smoke(): void {
        $this->assertTrue( true );
    }
}
PHP

cat > "${TMPDIR}/wp-codebox-core-stub.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
recipe_path=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--recipe" ]; then
        shift
        recipe_path="${1:-}"
    fi
    shift || true
done
if [ -n "${WP_CODEBOX_ARGS_FILE:-}" ] && [ -f "$recipe_path" ]; then
    cat "$recipe_path" > "$WP_CODEBOX_ARGS_FILE"
fi
core_src="$(jq -r '.inputs.mounts[] | select(.target == "/wordpress") | .source' "$recipe_path" | head -n 1)"
printf 'STAGE_BEGIN:run_tests\nALL TESTS PASSED\nTESTS: 1 FAILURES: 0 ERRORS: 0\n' > "${core_src}/.pg-test-result.txt"
printf '{"success":true,"executions":[{"command":"wordpress.core-phpunit","stdout":"OK (1 test, 1 assertion)\n","stderr":""}]}\n'
SH
chmod +x "${TMPDIR}/wp-codebox-core-stub.sh"

WP_CODEBOX_ARGS_FILE="${TMPDIR}/core-wp-codebox-recipe.json" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="wordpress-develop" \
HOMEBOY_COMPONENT_PATH="$CORE_WP_CODEBOX_FIXTURE" \
HOMEBOY_COMPONENT_SHAPE="core-dev" \
HOMEBOY_WORDPRESS_MULTISITE=1 \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/wp-codebox-core-stub.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/phpunit/tests/basic.php > "${TMPDIR}/test-wp-codebox.out"
assert_contains "${TMPDIR}/test-wp-codebox.out" "Running WordPress core PHPUnit tests via WP Codebox"
assert_contains "${TMPDIR}/test-wp-codebox.out" "WordPress core WP Codebox test run complete"
assert_contains "${TMPDIR}/core-wp-codebox-recipe.json" "wordpress.core-phpunit"
assert_contains "${TMPDIR}/core-wp-codebox-recipe.json" '"target": "/wordpress"'
assert_contains "${TMPDIR}/core-wp-codebox-recipe.json" '"target": "/wordpress/tests/phpunit"'
assert_contains "${TMPDIR}/core-wp-codebox-recipe.json" 'test-file=tests/phpunit/tests/basic.php'
assert_contains "${TMPDIR}/core-wp-codebox-recipe.json" 'multisite=1'
assert_not_contains "${TMPDIR}/core-wp-codebox-recipe.json" '"target": "/wordpress/vendor"'

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="wordpress-develop" \
HOMEBOY_COMPONENT_PATH="$FIXTURE" \
HOMEBOY_COMPONENT_SHAPE="core-dev" \
HOMEBOY_CORE_DEV_DRY_RUN=1 \
    bash "${EXTENSION_PATH}/scripts/lint/lint-runner.sh" > "${TMPDIR}/lint.out"
assert_contains "${TMPDIR}/lint.out" "core-dev lint runner selected"

LINT_FIXTURE="${TMPDIR}/core-lint-fixture"
prepare_core_lint_fixture "$LINT_FIXTURE"
run_core_lint_fixture "$LINT_FIXTURE" env > "${TMPDIR}/lint-success.out"
assert_contains "${TMPDIR}/lint-success.out" "Core-dev lint run complete"

HOMEBOY_FAKE_PHPSTAN_EXIT=1 run_core_lint_fixture "$LINT_FIXTURE" env > "${TMPDIR}/lint-phpstan-fail.out" 2>&1 && {
    echo "Expected core-dev lint to fail when PHPStan fails" >&2
    exit 1
}
assert_contains "${TMPDIR}/lint-phpstan-fail.out" "Core-dev lint run failed"

rm -f "${LINT_FIXTURE}/vendor/bin/phpstan"
run_core_lint_fixture "$LINT_FIXTURE" env > "${TMPDIR}/lint-missing-phpstan.out" 2>&1 && {
    echo "Expected core-dev lint to fail when PHPStan is missing" >&2
    exit 1
}
assert_contains "${TMPDIR}/lint-missing-phpstan.out" "PHPStan config or binary missing"

(
    cd "$FIXTURE"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="wordpress-develop" \
    HOMEBOY_COMPONENT_SHAPE="core-dev" \
    HOMEBOY_CORE_DEV_DRY_RUN=1 \
        bash "${EXTENSION_PATH}/scripts/build/build.sh" > "${TMPDIR}/build.out"
)
assert_contains "${TMPDIR}/build.out" "core-dev build runner selected"

echo "core-dev shape smoke passed"
