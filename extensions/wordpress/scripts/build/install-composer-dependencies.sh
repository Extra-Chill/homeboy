#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${EXTENSION_PATH}"

echo "Installing PHP dependencies..."

# Concurrent setups (an extension update racing a test run's hydration) must
# not replace vendor/ underneath each other.
exec 9>"${EXTENSION_PATH}/.composer-install.lock"
if command -v flock >/dev/null 2>&1; then
    flock 9
fi

# Composer trusts installed package metadata and will not restore arbitrary
# missing or truncated package files, so rebuild from the lockfile. Build the
# fresh tree in a staging directory and only replace vendor/ once it passes the
# integrity checks: a failed or interrupted install keeps the previous working
# vendor/ instead of leaving the extension without its PHPUnit harness.
STAGING_DIR="vendor.staging-$$"
RETIRED_DIR="vendor.retired-$$"
rm -rf vendor.staging-* vendor.retired-*
trap 'rm -rf "${EXTENSION_PATH}/${STAGING_DIR}"' EXIT

COMPOSER_VENDOR_DIR="${STAGING_DIR}" composer install --quiet --no-interaction --prefer-dist

verify_vendor() {
    local vendor_dir="$1"

    php -r '
$loader = require $argv[1];
if (! $loader instanceof Composer\Autoload\ClassLoader) {
    fwrite(STDERR, "Composer autoload did not return a ClassLoader.\n");
    exit(1);
}
' "${vendor_dir}/autoload.php"

    for binary in phpcs phpstan; do
        if [[ ! -x "${vendor_dir}/bin/${binary}" ]]; then
            echo "Composer dependency integrity check failed: vendor/bin/${binary} is missing or not executable." >&2
            exit 1
        fi
        "${vendor_dir}/bin/${binary}" --version >/dev/null
    done

    if [[ ! -f "${vendor_dir}/wp-phpunit/wp-phpunit/wp-tests-config.php" ]]; then
        echo "Composer dependency integrity check failed: the WP Codebox PHPUnit harness is incomplete." >&2
        exit 1
    fi
}

verify_vendor "${EXTENSION_PATH}/${STAGING_DIR}"

if [[ -e vendor ]]; then
    mv vendor "${RETIRED_DIR}"
fi
mv "${STAGING_DIR}" vendor
rm -rf "${RETIRED_DIR}"

echo "PHP dependency integrity checks passed."
