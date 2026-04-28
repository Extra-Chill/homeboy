#!/bin/bash
set -euo pipefail

# Setup script for WordPress Homeboy extension.
#
# Installs npm dependencies (including @wp-playground/cli for the default
# Playground test backend) and PHP dev dependencies (PHPCS, PHPStan for linting).
#
# The legacy wp-phpunit dependency was removed in Phase 3 (#214) — WordPress
# PHPUnit execution now runs inside Playground. The host-smoke backend is only
# for standalone PHP smoke scripts.

EXTENSION_PATH="$(pwd)"

echo "Setting up WordPress extension..."

# Install PHP dev dependencies (PHPCS, PHPStan, PHPUnit — used for linting
# and the extension's own self-tests, not for running component tests).
if [ -f "composer.json" ]; then
    echo "Installing PHP dependencies..."
    composer install --quiet --no-interaction

    if [ -x "vendor/bin/phpcs" ]; then
        echo "Registering PHPCS standards..."
        phpcs_paths=()
        for path in \
            "${EXTENSION_PATH}/vendor/wp-coding-standards/wpcs" \
            "${EXTENSION_PATH}/vendor/phpcsstandards/phpcsextra" \
            "${EXTENSION_PATH}/vendor/phpcsstandards/phpcsutils" \
            "${EXTENSION_PATH}/HomeboyWordPress"; do
            if [ -d "$path" ]; then
                phpcs_paths+=("$path")
            fi
        done

        if [ "${#phpcs_paths[@]}" -gt 0 ]; then
            installed_paths=$(IFS=','; printf '%s' "${phpcs_paths[*]}")
            vendor/bin/phpcs --config-set installed_paths "$installed_paths" --quiet > /dev/null 2>&1
        fi
    fi
fi

# Install npm dependencies (Playground CLI, ESLint).
if [ -f "package.json" ]; then
    echo "Installing npm dependencies (including @wp-playground/cli)..."
    npm install --quiet --no-fund --no-audit 2>&1 || {
        echo "Warning: npm install failed — Playground test backend will not be available"
    }
fi

echo "WordPress extension setup complete."
echo "Default test backend: Playground (PHP-WASM + embedded SQLite)"
echo "Host smoke backend: set test_backend=host-smoke for standalone tests/**/*-smoke.php"
