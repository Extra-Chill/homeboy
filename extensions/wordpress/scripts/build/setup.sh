#!/bin/bash
set -euo pipefail

# Derive extension path from current working directory
EXTENSION_PATH="$(pwd)"

echo "Setting up WordPress test infrastructure..."

# Install PHP dependencies (host backend: wp-phpunit, phpcs, phpunit)
cd "$EXTENSION_PATH"
composer install --quiet --no-interaction

# Install npm dependencies (ESLint + Playground CLI)
if [ -f "package.json" ]; then
    echo "Installing npm dependencies (ESLint + @wp-playground/cli)..."
    npm install --quiet --no-fund --no-audit 2>&1 || {
        echo "Warning: npm install failed, ESLint and Playground backend will be unavailable"
    }
fi

echo "WordPress test infrastructure installed successfully"
echo ""
echo "Host backend (default):"
echo "  WP_TESTS_DIR: $EXTENSION_PATH/vendor/wp-phpunit/wp-phpunit/tests/phpunit"
echo "  ABSPATH: cached WordPress (downloaded on first run)"
echo ""
echo "Playground backend (opt-in):"
echo "  CLI: $EXTENSION_PATH/node_modules/.bin/wp-playground"
echo "  Activate: homeboy component set <id> test_backend playground"
