#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${EXTENSION_PATH}"

echo "Installing PHP dependencies..."

# Composer trusts installed package metadata and will not restore arbitrary
# missing or truncated package files. Rebuild from the lockfile so extension
# updates cannot retain a partially copied vendor tree.
rm -rf vendor
composer install --quiet --no-interaction --prefer-dist

php -r '
$loader = require $argv[1];
if (! $loader instanceof Composer\Autoload\ClassLoader) {
    fwrite(STDERR, "Composer autoload did not return a ClassLoader.\n");
    exit(1);
}
' "${EXTENSION_PATH}/vendor/autoload.php"

for binary in phpcs phpstan; do
    if [[ ! -x "${EXTENSION_PATH}/vendor/bin/${binary}" ]]; then
        echo "Composer dependency integrity check failed: vendor/bin/${binary} is missing or not executable." >&2
        exit 1
    fi
    "${EXTENSION_PATH}/vendor/bin/${binary}" --version >/dev/null
done

if [[ ! -f "${EXTENSION_PATH}/vendor/wp-phpunit/wp-phpunit/wp-tests-config.php" ]]; then
    echo "Composer dependency integrity check failed: the WP Codebox PHPUnit harness is incomplete." >&2
    exit 1
fi

echo "PHP dependency integrity checks passed."
