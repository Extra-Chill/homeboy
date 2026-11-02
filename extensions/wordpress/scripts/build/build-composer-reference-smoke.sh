#!/usr/bin/env bash
# A plugin's production package depends on its content, not on the commit
# that built it. Composer records the root package's git commit in
# vendor/composer/installed.php unless it is given a root version, which would
# make every commit look like a content change to deploy selection.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"

if ! command -v composer >/dev/null 2>&1; then
    echo "SKIP: composer is not installed"
    exit 0
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-composer-reference.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
dir="${TMP_DIR}/reference-smoke"
mkdir -p "${dir}/src"
cat > "${dir}/reference-smoke.php" <<'PHP'
<?php
/**
 * Plugin Name: Reference Smoke
 * Version: 1.2.3
 */
PHP
printf '{"name":"smoke/reference-smoke","autoload":{"psr-4":{"Smoke\\\\":"src/"}}}\n' > "${dir}/composer.json"

git_fixture() {
    git -C "$dir" -c user.name=Fixture -c user.email=fixture@example.com "$@"
}

build_installed_php() {
    (
        cd "$dir"
        HOMEBOY_BUILD_SOURCE_DISPOSABLE=1 \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        HOMEBOY_COMPONENT_ID="reference-smoke" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
        HOMEBOY_SKIP_TESTS=1 \
            bash "${EXTENSION_DIR}/scripts/build/build.sh"
    ) > "${TMP_DIR}/build.out" 2>&1 || {
        sed 's/^/  | /' "${TMP_DIR}/build.out" >&2
        echo "FAIL: build failed" >&2
        exit 1
    }
    cat "${dir}/vendor/composer/installed.php"
}

git_fixture init -q
git_fixture add .
git_fixture commit -qm first
first="$(build_installed_php)"
git_fixture commit -q --allow-empty -m second
second="$(build_installed_php)"

if [ "$first" != "$second" ]; then
    echo "FAIL: installed.php changed between commits with identical content" >&2
    diff <(printf '%s\n' "$first") <(printf '%s\n' "$second") >&2 || true
    exit 1
fi
if ! grep -q "'pretty_version' => '1.2.3'" <<<"$first"; then
    echo "FAIL: root package version is not the plugin release version" >&2
    printf '%s\n' "$first" >&2
    exit 1
fi
echo "PASS: production installed.php is independent of the building commit"
