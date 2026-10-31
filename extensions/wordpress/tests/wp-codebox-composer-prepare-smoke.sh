#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
printf '%s\n' "$*" >> "${FIXTURE_COMPOSER_CAPTURE}"
exit 0
SH
chmod +x "${BIN_DIR}/composer"

cat > "${BIN_DIR}/wp-codebox" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' '{"executions":[{"stdout":"OK (1 test, 1 assertion)"}]}'
exit 0
SH
chmod +x "${BIN_DIR}/wp-codebox"

cat > "${WORK_DIR}/resolve-context.sh" <<SH
homeboy_resolve_context() {
    PLUGIN_PATH='${PLUGIN_DIR}'
    EXTENSION_PATH='${ROOT_DIR}'
    COMPONENT_ID='fixture-plugin'
}
SH

PATH="${BIN_DIR}:${PATH}" \
FIXTURE_COMPOSER_CAPTURE="$CAPTURE_FILE" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="${WORK_DIR}/resolve-context.sh" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="${ROOT_DIR}/tests/fixtures/wp-codebox-core-recipe-builder.mjs" \
bash "${ROOT_DIR}/scripts/test/test-runner-wp-codebox.sh" >/dev/null

if ! grep -q -- 'install --no-dev --no-interaction --no-progress --prefer-dist' "$CAPTURE_FILE"; then
    echo "Expected stale dev autoload to be regenerated with composer install --no-dev." >&2
    exit 1
fi

echo "wp-codebox composer prepare smoke passed"
