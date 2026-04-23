#!/bin/bash
set -euo pipefail

# Setup script for WordPress Homeboy extension.
#
# Installs npm dependencies (including @wp-playground/cli for the Playground
# test backend) and PHP dev dependencies (PHPCS, PHPStan for linting).
#
# The legacy wp-phpunit dependency was removed in Phase 3 (#214) — test
# execution now runs entirely inside WordPress Playground.

EXTENSION_PATH="$(pwd)"

echo "Setting up WordPress extension..."

# Install PHP dev dependencies (PHPCS, PHPStan, PHPUnit — used for linting
# and the extension's own self-tests, not for running component tests).
if [ -f "composer.json" ]; then
    echo "Installing PHP dependencies..."
    composer install --quiet --no-interaction
fi

# Install npm dependencies (Playground CLI, ESLint).
if [ -f "package.json" ]; then
    echo "Installing npm dependencies (including @wp-playground/cli)..."
    npm install --quiet --no-fund --no-audit 2>&1 || {
        echo "Warning: npm install failed — Playground test backend will not be available"
    }
fi

echo "WordPress extension setup complete."
echo "Test backend: Playground (PHP-WASM + embedded SQLite)"
