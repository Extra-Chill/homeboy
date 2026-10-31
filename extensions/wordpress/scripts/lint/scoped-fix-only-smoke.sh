#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
OUTPUT_FILE="${TMPDIR}/lint-output.txt"
TARGET_FILE="${COMPONENT_DIR}/inc/Changed.php"
UNRELATED_FILE="${COMPONENT_DIR}/inc/Unrelated.php"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${EXTENSION_DIR}/scripts/lint/php-fixers" "${EXTENSION_DIR}/rulesets" "${COMPONENT_DIR}/inc"
touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/component.php" <<'PHP'
<?php
/**
 * Plugin Name: Scoped Fix Fixture
 * Text Domain: scoped-fix-fixture
 */
PHP

cat > "$TARGET_FILE" <<'PHP'
<?php
function changed_fixture( $default ) {
    return $default;
}
PHP

cat > "$UNRELATED_FILE" <<'PHP'
<?php
function unrelated_fixture( $default ) {
    return $default;
}
PHP

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

for arg in "$@"; do
    if [ "$arg" = "--config-set" ]; then
        exit 0
    fi
done

exit 0
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

cat > "${EXTENSION_DIR}/scripts/lint/php-fixers/reserved-param-fixer.php" <<'PHP'
<?php
$path = $argv[1] ?? '';
if (is_dir($path)) {
    foreach (new RecursiveIteratorIterator(new RecursiveDirectoryIterator($path)) as $file) {
        if ($file->isFile() && substr($file->getFilename(), -4) === '.php') {
            file_put_contents($file->getPathname(), "// touched by reserved fixer\n", FILE_APPEND);
        }
    }
} elseif (is_file($path)) {
    file_put_contents($path, "// touched by reserved fixer\n", FILE_APPEND);
}
echo "Reserved param fixer: Fixed 1 parameter(s) in 1 file(s)\n";
PHP

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_TEXT_DOMAIN="scoped-fix-fixture" \
HOMEBOY_FIX_ONLY=1 \
HOMEBOY_STEP="phpcs" \
HOMEBOY_LINT_FILE="inc/Changed.php" \
    "$RUNNER" >"$OUTPUT_FILE" 2>&1
runner_exit=$?
set -e

if [ "$runner_exit" -ne 0 ]; then
    echo "FAIL: scoped fix-only runner exited ${runner_exit}" >&2
    sed 's/^/  /' "$OUTPUT_FILE" >&2
    exit 1
fi

if ! grep -Fq "Skipping reserved-param fixer for scoped lint fix" "$OUTPUT_FILE"; then
    echo "FAIL: scoped fix-only mode should skip reserved-param fixer" >&2
    sed 's/^/  /' "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -Fq "touched by reserved fixer" "$UNRELATED_FILE"; then
    echo "FAIL: scoped fix-only mode dirtied an unrelated file" >&2
    sed 's/^/  /' "$UNRELATED_FILE" >&2
    exit 1
fi

echo "scoped fix-only smoke passed"
