#!/bin/bash
set -euo pipefail

# Derive module path from current working directory
MODULE_PATH="$(pwd)"

echo "Setting up WordPress test infrastructure..."

# Install PHP dependencies
cd "$MODULE_PATH"
composer install --quiet --no-interaction

# Install npm dependencies for ESLint
if [ -f "package.json" ]; then
    echo "Installing ESLint dependencies..."
    npm install --quiet --no-fund --no-audit 2>&1 || {
        echo "Warning: npm install failed, ESLint linting will be skipped"
    }
fi

echo "WordPress test infrastructure installed successfully"
echo "WP_TESTS_DIR: $MODULE_PATH/vendor/wp-phpunit/wp-phpunit/tests/phpunit"
echo "ABSPATH: $MODULE_PATH/vendor/wp-phpunit/wp-phpunit/wordpress"
