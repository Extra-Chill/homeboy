#!/usr/bin/env bash
set -euo pipefail

# Audit reference dependency setup for WordPress extension.
#
# Resolves WordPress core + plugin dependencies and exports
# HOMEBOY_AUDIT_REFERENCE_PATHS so homeboy's audit can include them
# in cross-reference analysis (dead code detection).
#
# Usage:
#   source scripts/audit/setup-references.sh
#   homeboy audit <component>
#
# Or in CI:
#   eval "$(bash scripts/audit/setup-references.sh --export)"
#
# This reuses the same WordPress core cache as the test runner.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=../lib/validation-dependencies.sh
source "$EXTENSION_PATH/scripts/lib/validation-dependencies.sh"

# Resolve plugin path from homeboy context
PLUGIN_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
if [ ! -d "$PLUGIN_PATH" ]; then
    echo "Warning: Plugin path not found: $PLUGIN_PATH" >&2
    exit 0
fi

# Determine WordPress version from composer.lock or fallback
WP_VERSION=""
if [ -f "${EXTENSION_PATH}/composer.lock" ]; then
    WP_VERSION=$(grep -A5 '"name": "wp-phpunit/wp-phpunit"' "${EXTENSION_PATH}/composer.lock" \
        | grep '"version"' | head -1 \
        | sed 's/.*"version": "\([^"]*\)".*/\1/' || true)
fi
if [ -z "$WP_VERSION" ]; then
    WP_VERSION=$(composer show wp-phpunit/wp-phpunit --working-dir="${EXTENSION_PATH}" 2>/dev/null \
        | grep '^versions' | awk '{print $NF}' || echo "6.9.1")
fi

# WordPress core cache directory (shared with test runner)
WP_CACHE_BASE="${HOMEBOY_CACHE_DIR:-${HOME}/.cache/homeboy}/wordpress"
WP_CACHE_DIR="${WP_CACHE_BASE}/${WP_VERSION}"
WP_CORE_PATH="${WP_CACHE_DIR}/wordpress"

# Download WordPress if not cached
if [ ! -f "${WP_CORE_PATH}/wp-includes/version.php" ]; then
    echo "Downloading WordPress ${WP_VERSION} for audit references..." >&2
    mkdir -p "${WP_CACHE_DIR}"

    WP_DOWNLOAD_URL="https://wordpress.org/wordpress-${WP_VERSION}.tar.gz"

    if command -v curl &> /dev/null; then
        if ! curl -sL "$WP_DOWNLOAD_URL" | tar xz -C "${WP_CACHE_DIR}"; then
            echo "Warning: Failed to download WordPress ${WP_VERSION}" >&2
            exit 0
        fi
    elif command -v wget &> /dev/null; then
        if ! wget -qO- "$WP_DOWNLOAD_URL" | tar xz -C "${WP_CACHE_DIR}"; then
            echo "Warning: Failed to download WordPress ${WP_VERSION}" >&2
            exit 0
        fi
    else
        echo "Warning: Neither curl nor wget available for WordPress download" >&2
        exit 0
    fi
    echo "WordPress ${WP_VERSION} cached at ${WP_CORE_PATH}" >&2
fi

# Build reference paths: WordPress core + plugin dependencies
REFERENCE_PATHS="${WP_CORE_PATH}"

# Resolve plugin dependencies via validation-dependencies.sh
DEP_PATHS=$(homeboy_resolve_validation_dependency_paths "$PLUGIN_PATH" 2>/dev/null || true)
if [ -n "$DEP_PATHS" ]; then
    while IFS= read -r dep_path; do
        [ -n "$dep_path" ] && [ -d "$dep_path" ] && REFERENCE_PATHS="${REFERENCE_PATHS}"$'\n'"${dep_path}"
    done <<< "$DEP_PATHS"
fi

export HOMEBOY_AUDIT_REFERENCE_PATHS="$REFERENCE_PATHS"

# Count paths for logging
PATH_COUNT=$(echo "$REFERENCE_PATHS" | wc -l | tr -d ' ')
echo "Audit reference dependencies: ${PATH_COUNT} path(s) resolved (WP ${WP_VERSION})" >&2

# If --export flag passed, print the export statement for eval
if [ "${1:-}" = "--export" ]; then
    # Escape newlines for shell export
    printf 'export HOMEBOY_AUDIT_REFERENCE_PATHS=%q\n' "$REFERENCE_PATHS"
fi
