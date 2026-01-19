#!/bin/bash
set -euo pipefail

# Derive module path from current working directory
MODULE_PATH="$(pwd)"

echo "Setting up WordPress test infrastructure..."

# Install dependencies
cd "$MODULE_PATH"
composer install --quiet --no-interaction

echo "WordPress test infrastructure installed successfully"
echo "WP_TESTS_DIR: $MODULE_PATH/vendor/wp-phpunit/wp-phpunit/tests/phpunit"
echo "ABSPATH: $MODULE_PATH/vendor/wp-phpunit/wp-phpunit/wordpress"
