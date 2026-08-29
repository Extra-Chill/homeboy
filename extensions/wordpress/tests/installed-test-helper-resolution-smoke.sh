#!/usr/bin/env bash
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOSITORY_ROOT="$(cd "${WORDPRESS_ROOT}/.." && pwd)"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

mkdir -p \
    "${FIXTURE_ROOT}/extension-sources/wordpress/scripts/test" \
    "${FIXTURE_ROOT}/extension-sources/wordpress/scripts/lib" \
    "${FIXTURE_ROOT}/extensions/scripts/lib" \
    "${FIXTURE_ROOT}/component"
cp "${WORDPRESS_ROOT}/scripts/test/test-runner.sh" \
    "${WORDPRESS_ROOT}/scripts/test/parse-test-results.sh" \
    "${FIXTURE_ROOT}/extension-sources/wordpress/scripts/test/"
cp "${WORDPRESS_ROOT}/scripts/lib/validation-dependencies.sh" \
    "${FIXTURE_ROOT}/extension-sources/wordpress/scripts/lib/"
cp "${REPOSITORY_ROOT}/scripts/lib/test-result-adapters.sh" \
    "${REPOSITORY_ROOT}/scripts/lib/runner-harness.sh" \
    "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh" \
    "${FIXTURE_ROOT}/extensions/scripts/lib/"

# The settings helper is core-owned. Resolve the real one and hand it over the
# way Homeboy does at runtime, instead of vendoring a second copy into the
# fixture.
# shellcheck source=../../scripts/lib/runtime-helper-resolver.sh
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
SETTINGS_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_SETTINGS_HELPER settings.sh)" || exit 1
ln -s "${FIXTURE_ROOT}/extension-sources/wordpress" "${FIXTURE_ROOT}/extensions/wordpress"

cat > "${FIXTURE_ROOT}/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH}"
}
EOF

HOMEBOY_EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress" \
HOMEBOY_COMPONENT_PATH="${FIXTURE_ROOT}/component" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="${FIXTURE_ROOT}/runner-prelude.sh" \
HOMEBOY_RUNTIME_SETTINGS_HELPER="$SETTINGS_HELPER" \
    bash "${FIXTURE_ROOT}/extensions/wordpress/scripts/test/test-runner.sh" --help >/dev/null

printf '%s\n' 'OK (1 test, 1 assertion)' > "${FIXTURE_ROOT}/phpunit-output.txt"
HOMEBOY_EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress" \
    bash "${FIXTURE_ROOT}/extensions/wordpress/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/phpunit-output.txt" phpunit

HOMEBOY_EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress" \
HOMEBOY_SETTINGS_JSON='{}' \
HOMEBOY_RUNTIME_SETTINGS_HELPER="$SETTINGS_HELPER" \
    bash -c 'source "$1"; homeboy_get_validation_dependencies_raw' bash \
        "${FIXTURE_ROOT}/extensions/wordpress/scripts/lib/validation-dependencies.sh" >/dev/null

printf '%s\n' 'installed test helper resolution smoke passed'
