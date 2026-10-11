#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_SCRIPT="${ROOT_DIR}/scripts/build/install-composer-dependencies.sh"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-composer-vendor.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

EXTENSION_DIR="${WORK_DIR}/wordpress"
BIN_DIR="${WORK_DIR}/bin"
mkdir -p "${EXTENSION_DIR}/scripts/build" "${EXTENSION_DIR}/vendor/composer" "${BIN_DIR}"
cp "${INSTALL_SCRIPT}" "${EXTENSION_DIR}/scripts/build/install-composer-dependencies.sh"
printf '%s\n' '{}' > "${EXTENSION_DIR}/composer.json"
printf '%s\n' '{}' > "${EXTENSION_DIR}/composer.lock"
printf '%s\n' 'stale package metadata' > "${EXTENSION_DIR}/vendor/composer/installed.php"
printf '%s\n' 'truncated package payload' > "${EXTENSION_DIR}/vendor/.incomplete"

cat > "${BIN_DIR}/composer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

vendor="${COMPOSER_VENDOR_DIR:-vendor}"
if [[ -e "${vendor}/.incomplete" || -e "${vendor}/composer/installed.php" ]]; then
    echo "stale vendor tree survived into composer install" >&2
    exit 91
fi

printf '%s\n' "$*" > "${FAKE_COMPOSER_ARGS}"
if [[ "${FAKE_COMPOSER_FAIL:-0}" == "1" ]]; then
    mkdir -p "${vendor}/partial"
    echo "network failure mid-install" >&2
    exit 1
fi
mkdir -p "${vendor}/composer" "${vendor}/bin" "${vendor}/wp-phpunit/wp-phpunit"

cat > "${vendor}/composer/ClassLoader.php" <<'PHP'
<?php
namespace Composer\Autoload;
class ClassLoader {}
PHP

cat > "${vendor}/autoload.php" <<'PHP'
<?php
require __DIR__ . '/composer/ClassLoader.php';
return new Composer\Autoload\ClassLoader();
PHP

cat > "${vendor}/bin/phpcs" <<'SH_BIN'
#!/usr/bin/env bash
echo "PHP_CodeSniffer fixture"
SH_BIN

cat > "${vendor}/bin/phpstan" <<'SH_BIN'
#!/usr/bin/env bash
# Real PHPStan also loads ./vendor/autoload.php from the working directory.
# When that is a different tree with the same autoloader class, PHP fatals.
own_vendor="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -f "${PWD}/vendor/autoload.php" && "$(cd "${PWD}/vendor" && pwd)" != "${own_vendor}" ]]; then
    echo "PHP Fatal error:  Cannot declare class ComposerAutoloaderInitFixture, because the name is already in use" >&2
    exit 255
fi
if [[ "${FAKE_CORRUPT_PHPSTAN:-0}" == "1" ]]; then
    echo "corrupted phpstan.phar" >&2
    exit 2
fi
echo "PHPStan fixture"
SH_BIN

chmod +x "${vendor}/bin/phpcs" "${vendor}/bin/phpstan"
printf '%s\n' '<?php' > "${vendor}/wp-phpunit/wp-phpunit/wp-tests-config.php"
SH
chmod +x "${BIN_DIR}/composer"

FAKE_COMPOSER_ARGS="${WORK_DIR}/composer-args.txt" \
PATH="${BIN_DIR}:${PATH}" \
    bash "${EXTENSION_DIR}/scripts/build/install-composer-dependencies.sh"

if [[ -e "${EXTENSION_DIR}/vendor/.incomplete" ]]; then
    echo "Incomplete vendor payload survived the clean install." >&2
    exit 1
fi

if ! grep -q -- 'install --quiet --no-interaction --prefer-dist' "${WORK_DIR}/composer-args.txt"; then
    echo "Expected a lockfile-backed Composer install." >&2
    exit 1
fi

# Re-running over a healthy, already-installed vendor/ must succeed: the staged
# tree is verified while the live tree still exists next to it (#2932).
if ! FAKE_COMPOSER_ARGS="${WORK_DIR}/composer-args.txt" \
    PATH="${BIN_DIR}:${PATH}" \
        bash "${EXTENSION_DIR}/scripts/build/install-composer-dependencies.sh" >/dev/null 2>&1; then
    echo "Re-installing over an existing healthy vendor/ failed (staged and live autoloaders collided)." >&2
    exit 1
fi

if FAKE_COMPOSER_ARGS="${WORK_DIR}/composer-args.txt" \
    FAKE_CORRUPT_PHPSTAN=1 \
    PATH="${BIN_DIR}:${PATH}" \
        bash "${EXTENSION_DIR}/scripts/build/install-composer-dependencies.sh" >/dev/null 2>&1; then
    echo "A corrupt PHPStan executable passed the vendor integrity check." >&2
    exit 1
fi

if [[ ! -f "${EXTENSION_DIR}/vendor/wp-phpunit/wp-phpunit/wp-tests-config.php" ]]; then
    echo "A corrupt PHPStan install replaced the previous working vendor tree." >&2
    exit 1
fi

if FAKE_COMPOSER_ARGS="${WORK_DIR}/composer-args.txt" \
    FAKE_COMPOSER_FAIL=1 \
    PATH="${BIN_DIR}:${PATH}" \
        bash "${EXTENSION_DIR}/scripts/build/install-composer-dependencies.sh" >/dev/null 2>&1; then
    echo "A failed Composer install reported success." >&2
    exit 1
fi

if [[ ! -f "${EXTENSION_DIR}/vendor/wp-phpunit/wp-phpunit/wp-tests-config.php" ]]; then
    echo "A failed Composer install removed the previous working vendor tree." >&2
    exit 1
fi

if compgen -G "${EXTENSION_DIR}/vendor.staging-*" >/dev/null || compgen -G "${EXTENSION_DIR}/vendor.retired-*" >/dev/null; then
    echo "A failed Composer install left staging or retired vendor directories behind." >&2
    exit 1
fi

echo "WordPress extension Composer vendor integrity smoke passed."
