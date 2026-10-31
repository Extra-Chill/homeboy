#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

EXTENSION_DIR="${TMP_DIR}/extension"
COMPONENT_DIR="${TMP_DIR}/component"
PHPCS_ARGS_FILE="${TMP_DIR}/phpcs-args.txt"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${EXTENSION_DIR}/scripts/lint" "${EXTENSION_DIR}/rulesets" "${COMPONENT_DIR}/src" "${COMPONENT_DIR}/artifacts/generated"
touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Git Discovery Fixture
 * Text Domain: git-discovery-fixture
 */
PHP
cat > "${COMPONENT_DIR}/src/tracked-violation.php" <<'PHP'
<?php
echo $_GET['tracked'];
PHP
cat > "${COMPONENT_DIR}/artifacts/generated/ignored-violation.php" <<'PHP'
<?php
echo $_GET['ignored'];
PHP
printf '%s\n' 'artifacts/' > "${COMPONENT_DIR}/.gitignore"
git -C "$COMPONENT_DIR" init -q
git -C "$COMPONENT_DIR" add .gitignore plugin.php src/tracked-violation.php
git -C "$COMPONENT_DIR" -c user.name=fixture -c user.email=fixture@example.test commit -qm fixture

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$PHPCS_ARGS_FILE"
if [[ " $* " == *' --report=json '* ]]; then
    printf '%s\n' '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}'
fi
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

run_lint() {
    : > "$PHPCS_ARGS_FILE"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="git-discovery-fixture" \
    HOMEBOY_SUMMARY_MODE=1 \
    HOMEBOY_STEP="phpcs" \
    PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
        bash "$RUNNER" >/dev/null
}

assert_contains() {
    local expected="$1"
    if ! grep -Fqx -- "$expected" "$PHPCS_ARGS_FILE"; then
        echo "Expected PHPCS target: $expected" >&2
        exit 1
    fi
}

assert_not_contains() {
    local unexpected="$1"
    if grep -Fqx -- "$unexpected" "$PHPCS_ARGS_FILE"; then
        echo "Unexpected PHPCS target: $unexpected" >&2
        exit 1
    fi
}

run_lint
assert_contains "${COMPONENT_DIR}/src/tracked-violation.php"
assert_not_contains "${COMPONENT_DIR}/artifacts/generated/ignored-violation.php"

: > "$PHPCS_ARGS_FILE"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="git-discovery-fixture" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_STEP="phpcs" \
HOMEBOY_LINT_FILE="artifacts/generated/ignored-violation.php" \
PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
    bash "$RUNNER" >/dev/null
assert_contains "${COMPONENT_DIR}/artifacts/generated/ignored-violation.php"

: > "$PHPCS_ARGS_FILE"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="git-discovery-fixture" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_STEP="phpcs" \
HOMEBOY_LINT_GLOB="artifacts/generated/ignored-violation.php" \
PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
    bash "$RUNNER" >/dev/null
assert_contains "artifacts/generated/ignored-violation.php"

echo "WordPress Git-ignored lint discovery smoke passed"
