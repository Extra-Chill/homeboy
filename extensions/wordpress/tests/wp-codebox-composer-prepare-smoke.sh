#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Installed runs receive the runtime helper from Homeboy.
if [ -z "${HOMEBOY_RUNTIME_SETTINGS_HELPER:-}" ] && [ -f "$ROOT_DIR/../../crates/homeboy-core/src/extension/runtime/settings.sh" ]; then
    export HOMEBOY_RUNTIME_SETTINGS_HELPER="$ROOT_DIR/../../crates/homeboy-core/src/extension/runtime/settings.sh"
fi
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-composer-prepare.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

PLUGIN_DIR="${WORK_DIR}/fixture-plugin"
BIN_DIR="${WORK_DIR}/bin"
CAPTURE_FILE="${WORK_DIR}/composer-args.txt"
mkdir -p "${PLUGIN_DIR}/tests" "${PLUGIN_DIR}/vendor" "$BIN_DIR"

cat > "${PLUGIN_DIR}/fixture-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Fixture Plugin
 */
PHP

cat > "${PLUGIN_DIR}/tests/FixtureTest.php" <<'PHP'
<?php
class FixtureTest {}
PHP

cat > "${PLUGIN_DIR}/composer.json" <<'JSON'
{
  "autoload": {
    "psr-4": {
      "FixturePlugin\\": "src/"
    }
  },
  "autoload-dev": {
    "files": ["tests/dev-stubs.php"]
  }
}
JSON

printf '%s\n' '<?php // stale dev autoload' > "${PLUGIN_DIR}/vendor/autoload.php"

cat > "${BIN_DIR}/composer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
grep -Fxq '<?php // stale dev autoload' vendor/autoload.php
printf '%s\n' "$*" >> "${FIXTURE_COMPOSER_CAPTURE}"
printf '%s\n' '<?php // production autoload' > vendor/autoload.php
SH
chmod +x "${BIN_DIR}/composer"

cat > "${BIN_DIR}/wp-codebox" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    commands) ;;
    --version) printf '%s\n' '0.21.0' ;;
    runtime)
        [[ "$*" == 'runtime descriptor --json' ]]
        printf '%s\n' '{"schema":"wp-codebox/runtime-descriptor/v1","readiness":{"status":"available","browserRuntime":{"status":"ready"}},"contractManifest":{"schemas":{"runtimeBoundary":{"browserContainedSiteOpen":"wp-codebox/browser-contained-site-open/v1"}}}}'
        ;;
    recipe)
        grep -Fxq '<?php // production autoload' vendor/autoload.php
        while [[ "$1" != --output ]]; do shift; done
        printf '%s\n' '{"schema":"wp-codebox/workspace-recipe/v1"}' > "$2"
        ;;
    recipe-run)
        while [[ "$1" != --artifacts ]]; do shift; done
        mkdir -p "$2/runtime-fixture/files"
        printf '%s\n' '{"paths":{"runtimeDirectory":"runtime-fixture"}}' > "$2/latest-runtime.json"
        printf '%s\n' '{"schema":"wp-codebox/test-results/v1","status":"passed","summary":{"total":1,"passed":1,"failed":0,"skipped":0}}' > "$2/runtime-fixture/files/test-results.json"
        printf '%s\n' '{"success":true}'
        ;;
    *) exit 1 ;;
esac
SH
chmod +x "${BIN_DIR}/wp-codebox"

# The runner does not infer component dependency installs. Declare this
# fixture's production Composer refresh through the supported prepare hook.
PATH="${BIN_DIR}:${PATH}" \
FIXTURE_COMPOSER_CAPTURE="$CAPTURE_FILE" \
HOMEBOY_COMPONENT_PATH="$PLUGIN_DIR" \
HOMEBOY_COMPONENT_ID=fixture-plugin \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_prepare_steps":[{"command":"composer","args":["install","--no-dev","--no-interaction","--no-progress","--prefer-dist"]}]}' \
HOMEBOY_WP_CODEBOX_BIN="${BIN_DIR}/wp-codebox" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="${WORK_DIR}/artifacts" \
bash "${ROOT_DIR}/scripts/test/test-runner-wp-codebox.sh"

if ! grep -Fxq -- 'install --no-dev --no-interaction --no-progress --prefer-dist' "$CAPTURE_FILE"; then
    echo "Expected stale dev autoload to be regenerated with composer install --no-dev." >&2
    exit 1
fi

echo "wp-codebox composer prepare smoke passed"
