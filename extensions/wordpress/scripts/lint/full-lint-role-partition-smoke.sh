#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

EXTENSION_DIR="${TMP_DIR}/extension"
COMPONENT_DIR="${TMP_DIR}/component"
PHPCS_ARGS_FILE="${TMP_DIR}/phpcs-args.txt"

PRODUCTION_REL="plugin.php"
HARNESS_REL="tests/smoke-deliberately-non-wpcs.php"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${EXTENSION_DIR}/scripts/lint" "${EXTENSION_DIR}/rulesets" "${COMPONENT_DIR}/tests"
touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/${PRODUCTION_REL}" <<'PHP'
<?php
/**
 * Plugin Name: Full Lint Role Partition Fixture
 * Text Domain: full-lint-role-partition-fixture
 */

echo esc_html( $_GET['partition'] );
PHP

# Deliberately non-WPCS standalone smoke harness: compact fixture style the
# production WordPress ruleset would flag, but valid PHP for php -l.
cat > "${COMPONENT_DIR}/${HARNESS_REL}" <<'PHP'
<?php
echo $_GET['harness'];
PHP

git -C "$COMPONENT_DIR" init -q
git -C "$COMPONENT_DIR" add .
git -C "$COMPONENT_DIR" -c user.name=fixture -c user.email=fixture@example.test commit -qm fixture

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" >> "$PHPCS_ARGS_FILE"
if [[ " $* " == *' --report=json '* ]]; then
    printf '%s\n' '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}'
fi
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

assert_args_contain() {
    local expected="$1"
    if ! grep -Fqx -- "$expected" "$PHPCS_ARGS_FILE"; then
        echo "FAIL: expected PHPCS argument: $expected" >&2
        echo "PHPCS arguments recorded:" >&2
        cat "$PHPCS_ARGS_FILE" >&2
        exit 1
    fi
}

assert_args_exclude() {
    local unexpected="$1"
    if grep -Fq -- "$unexpected" "$PHPCS_ARGS_FILE"; then
        echo "FAIL: unexpected PHPCS argument: $unexpected" >&2
        echo "PHPCS arguments recorded:" >&2
        cat "$PHPCS_ARGS_FILE" >&2
        exit 1
    fi
}

run_full_lint() {
    : > "$PHPCS_ARGS_FILE"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="full-lint-role-partition-fixture" \
    HOMEBOY_SUMMARY_MODE=1 \
    HOMEBOY_STEP="phpcs" \
    PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
        bash "$RUNNER" >/dev/null 2>&1
}

# Full-repository lint must partition by WordPress lint role: the production
# file runs through PHPCS, the test harness gets php -l only (#2799).
run_full_lint
assert_args_contain "${COMPONENT_DIR}/${PRODUCTION_REL}"
assert_args_exclude "${COMPONENT_DIR}/${HARNESS_REL}"

# Scoped glob behavior is unchanged: the partition applies to explicit globs
# too, so the harness stays on php -l and only the production file hits PHPCS.
: > "$PHPCS_ARGS_FILE"
(
    cd "$COMPONENT_DIR"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="full-lint-role-partition-fixture" \
    HOMEBOY_SUMMARY_MODE=1 \
    HOMEBOY_STEP="phpcs" \
    HOMEBOY_LINT_GLOB="{${PRODUCTION_REL},${HARNESS_REL}}" \
    PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
        bash "$RUNNER" >/dev/null 2>&1
)
assert_args_contain "${PRODUCTION_REL}"
assert_args_exclude "${HARNESS_REL}"

# The harness must still be syntax-gated outside the production profile: a
# broken harness fails the full lint without ever reaching PHPCS.
printf '%s\n' '<?php' 'function broken( {' > "${COMPONENT_DIR}/${HARNESS_REL}"
set +e
run_full_lint
broken_harness_rc=$?
set -e
if [ "$broken_harness_rc" -eq 0 ]; then
    echo "FAIL: full lint passed despite broken syntax in the non-runtime test harness" >&2
    exit 1
fi
assert_args_exclude "${HARNESS_REL}"

echo "Full lint role partition smoke passed"
