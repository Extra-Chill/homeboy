#!/usr/bin/env bash
set -euo pipefail

# Native lint runner for wordpress-develop / WordPress core source checkouts.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=../lib/resolve-context.sh
source "${RESOLVE_CONTEXT_HELPER}"
homeboy_resolve_context --component-alias PLUGIN_PATH

CORE_PATH="$PLUGIN_PATH"

fail() {
    echo "Error: $*" >&2
    exit 1
}

if [ ! -f "${CORE_PATH}/wp-config-sample.php" ] \
    || [ ! -f "${CORE_PATH}/src/wp-includes/version.php" ] \
    || [ ! -d "${CORE_PATH}/tests/phpunit" ]; then
    fail "core-dev lint runner expected wordpress-develop markers"
fi

if [ "${HOMEBOY_CORE_DEV_DRY_RUN:-}" = "1" ]; then
    echo "core-dev lint runner selected: ${CORE_PATH}"
    exit 0
fi

cd "$CORE_PATH"

PHPCS_BIN="${CORE_PATH}/vendor/bin/phpcs"
PHPCS_CONFIG="${CORE_PATH}/.phpcs.xml.dist"
PHPSTAN_BIN="${CORE_PATH}/vendor/bin/phpstan"
PHPSTAN_CONFIG="${CORE_PATH}/phpstan.neon.dist"
CHANGED_SINCE="${HOMEBOY_CHANGED_SINCE:-origin/trunk}"

mapfile -t CHANGED_PHP_FILES < <(
    git diff --name-only "$CHANGED_SINCE" -- '*.php' 2>/dev/null \
        | grep -Ev '^(vendor|node_modules|build)/' || true
)
mapfile -t CHANGED_JS_FILES < <(
    git diff --name-only "$CHANGED_SINCE" -- '*.js' '*.jsx' '*.ts' '*.tsx' 2>/dev/null \
        | grep -Ev '^(vendor|node_modules|build)/' || true
)

if [ "${#CHANGED_PHP_FILES[@]}" -eq 0 ]; then
    echo "No changed PHP files vs ${CHANGED_SINCE}; skipping PHPCS/PHPStan."
else
    if [ -x "$PHPCS_BIN" ] && [ -f "$PHPCS_CONFIG" ]; then
        echo "Running WordPress core PHPCS on ${#CHANGED_PHP_FILES[@]} changed PHP file(s)..."
        "$PHPCS_BIN" --standard="$PHPCS_CONFIG" "${CHANGED_PHP_FILES[@]}" || true
    else
        echo "Warning: WordPress core PHPCS config or binary missing; skipping PHPCS."
    fi

    if [ -x "$PHPSTAN_BIN" ] && [ -f "$PHPSTAN_CONFIG" ]; then
        phpstan_args=(analyse --configuration="$PHPSTAN_CONFIG" --memory-limit=2G)
        if [ "${HOMEBOY_STRICT_TYPES:-}" = "1" ]; then
            phpstan_args+=(--level=max)
        fi
        phpstan_args+=("${CHANGED_PHP_FILES[@]}")
        echo "Running WordPress core PHPStan on ${#CHANGED_PHP_FILES[@]} changed PHP file(s)..."
        "$PHPSTAN_BIN" "${phpstan_args[@]}" || true
    else
        echo "Warning: WordPress core PHPStan config or binary missing; skipping PHPStan."
    fi
fi

if [ "${#CHANGED_JS_FILES[@]}" -gt 0 ] && [ -f package.json ] && command -v npm >/dev/null 2>&1; then
    echo "Changed JS/TS files detected; running WordPress core JS lint script."
    npm run lint:js -- "${CHANGED_JS_FILES[@]}" || true
fi

echo "Core-dev lint run complete."
