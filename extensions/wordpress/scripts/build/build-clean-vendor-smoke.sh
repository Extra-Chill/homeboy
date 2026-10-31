#!/usr/bin/env bash
set -euo pipefail

# Smoke test for the Universal WordPress Build Script's clean-vendor behavior.
#
# Regression coverage for #2009: when a component is built from a dev checkout
# (or a copy of one) whose vendor/ holds a source/git-installed dependency, the
# build copy strips the per-package .git directories. A plain `composer install`
# then trips composer's GitDownloader on the now-.git-less source package and
# aborts ("The .git directory is missing from .../vendor/..."), producing no
# artifact and failing the release.
#
# The fix removes vendor/ before installing so the install is always a fresh,
# dist-based install independent of the dev checkout's state. This test proves
# the fix with a stubbed composer that mimics GitDownloader: it FAILS the
# install if a carried-in source-vendor marker is still present at install time,
# and otherwise produces a clean vendor/ tree. With the fix the build succeeds
# and the produced ZIP carries the clean vendor (no source remnant); without it
# the stub composer would abort exactly like the real GitDownloader.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-build-clean-vendor.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

fake_bin="${TMP_DIR}/bin"
mkdir -p "$fake_bin"

# Stub composer that mimics the real GitDownloader failure mode.
#
# `composer install` aborts (exit 1) if the carried-in source-vendor marker is
# still present — this is the .git-less source package the real GitDownloader
# chokes on. Otherwise it writes a clean, dist-style vendor/ tree and exits 0.
# Any non-install subcommand is a no-op success.
cat > "${fake_bin}/composer" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "install" ]; then
    if [ -e "vendor/.source-install-marker" ]; then
        echo "In GitDownloader.php line 155:" >&2
        echo "  The .git directory is missing from $(pwd)/vendor/symfony/deprecation-contracts" >&2
        exit 1
    fi
    rm -rf vendor
    mkdir -p vendor/composer vendor/symfony/deprecation-contracts
    cat > vendor/autoload.php <<'PHP'
<?php
// stub dist autoloader
require __DIR__ . '/composer/autoload_real.php';
PHP
    cat > vendor/composer/autoload_real.php <<'PHP'
<?php
// stub composer autoload_real
PHP
    cat > vendor/symfony/deprecation-contracts/function.php <<'PHP'
<?php
// stub dist-installed dependency (no .git, clean)
PHP
fi
exit 0
SH
chmod +x "${fake_bin}/composer"

# Component fixture whose dev vendor/ looks like a source/git install whose
# .git was stripped by the build copy: real package files are present, the
# per-package .git is gone, and the source-install marker remains.
component_dir="${TMP_DIR}/dev-stability-plugin"
mkdir -p "$component_dir/inc"
cat > "${component_dir}/dev-stability-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Dev Stability Plugin
 * Version: 1.0.0
 */
PHP

cat > "${component_dir}/composer.json" <<'JSON'
{
  "name": "smoke/dev-stability-plugin",
  "minimum-stability": "dev",
  "require": {
    "symfony/deprecation-contracts": "dev-main"
  }
}
JSON

# Pre-existing source vendor/ carried into the build (no .git, marker present).
mkdir -p "${component_dir}/vendor/symfony/deprecation-contracts"
cat > "${component_dir}/vendor/symfony/deprecation-contracts/function.php" <<'PHP'
<?php
// carried-in source install (its .git was stripped by the build copy)
PHP
: > "${component_dir}/vendor/.source-install-marker"

(
    cd "$component_dir"
    PATH="${fake_bin}:$PATH" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="dev-stability-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" >"${TMP_DIR}/build.log" 2>&1
) || {
    echo "----- build output -----" >&2
    sed 's/^/  /' "${TMP_DIR}/build.log" >&2
    fail "build aborted on a carried-in source vendor/ (GitDownloader regression #2009)"
}

zip_file="${component_dir}/build/dev-stability-plugin.zip"
[ -f "$zip_file" ] || fail "no production ZIP was produced"

zip_listing="$(unzip -Z1 "$zip_file")"

# The clean dist install must be in the ZIP.
printf '%s\n' "$zip_listing" | grep -Fq "dev-stability-plugin/vendor/autoload.php" \
    || fail "ZIP is missing the freshly installed vendor/autoload.php"

# The carried-in source remnant must be gone (vendor was reinstalled clean).
if printf '%s\n' "$zip_listing" | grep -Fq ".source-install-marker"; then
    fail "ZIP still contains the carried-in source-vendor marker (vendor was not cleaned)"
fi

# No .git remnants from any source package.
if printf '%s\n' "$zip_listing" | grep -Eq '(^|/)\.git(/|$)'; then
    fail "ZIP contains .git remnants"
fi

echo "WordPress build clean-vendor smoke passed."
