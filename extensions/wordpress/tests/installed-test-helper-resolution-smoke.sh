#!/usr/bin/env bash
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOSITORY_ROOT="$(cd "${WORDPRESS_ROOT}/.." && pwd)"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

mkdir -p \
    "${FIXTURE_ROOT}/extension-sources/wordpress/scripts/test" \
    "${FIXTURE_ROOT}/extensions/scripts/lib" \
    "${FIXTURE_ROOT}/component"
cp "${WORDPRESS_ROOT}/scripts/test/test-runner.sh" \
    "${WORDPRESS_ROOT}/scripts/test/parse-test-results.sh" \
    "${FIXTURE_ROOT}/extension-sources/wordpress/scripts/test/"
cp "${REPOSITORY_ROOT}/scripts/lib/settings.sh" \
    "${REPOSITORY_ROOT}/scripts/lib/test-result-adapters.sh" \
    "${FIXTURE_ROOT}/extensions/scripts/lib/"
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
    bash "${FIXTURE_ROOT}/extensions/wordpress/scripts/test/test-runner.sh" --help >/dev/null

printf '%s\n' 'OK (1 test, 1 assertion)' > "${FIXTURE_ROOT}/phpunit-output.txt"
HOMEBOY_EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress" \
    bash "${FIXTURE_ROOT}/extensions/wordpress/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/phpunit-output.txt" phpunit

printf '%s\n' 'installed test helper resolution smoke passed'
