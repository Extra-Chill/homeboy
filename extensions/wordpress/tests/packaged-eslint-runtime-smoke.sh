#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-packaged-eslint.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

HYDRATED_ROOT="${TMP_DIR}/extension-runtime"
mkdir -p "$HYDRATED_ROOT"

# Model the extension distribution and hydration boundary. The lint runtime is
# part of this artifact, so preserve its package tree and executable layout.
tar \
    --exclude='wordpress/vendor' \
    -C "$ROOT_DIR" -cf - wordpress scripts/lib \
    | tar -C "$HYDRATED_ROOT" -xf -

EXTENSION_PATH="${HYDRATED_ROOT}/wordpress"
mkdir -p "${TMP_DIR}/no-package-manager"
cat > "${TMP_DIR}/no-package-manager/npm" <<'SH'
#!/usr/bin/env bash
echo "The packaged runtime smoke must not install dependencies" >&2
exit 99
SH
chmod +x "${TMP_DIR}/no-package-manager/npm"

ESLINT_BIN="${EXTENSION_PATH}/node_modules/.bin/eslint"
"$ESLINT_BIN" --version > "${TMP_DIR}/eslint-version.out"
if ! grep -Eq '^v[0-9]+' "${TMP_DIR}/eslint-version.out"; then
    echo "Expected hydrated ESLint to report a version" >&2
    cat "${TMP_DIR}/eslint-version.out" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_PATH="${EXTENSION_PATH}/tests/fixtures/eslint-provider/default-only" \
HOMEBOY_COMPONENT_ID="eslint-provider-fixture" \
HOMEBOY_COMPONENT_TEXT_DOMAIN="eslint-provider-fixture" \
PATH="${TMP_DIR}/no-package-manager:${PATH}" \
    bash "${EXTENSION_PATH}/scripts/lint/eslint-runner.sh" > "${TMP_DIR}/fixture-lint.out" 2>&1

if ! grep -Fq 'ESLint linting passed' "${TMP_DIR}/fixture-lint.out"; then
    echo "Expected hydrated ESLint to lint the fixture successfully" >&2
    cat "${TMP_DIR}/fixture-lint.out" >&2
    exit 1
fi

echo "Packaged WordPress ESLint runtime smoke passed"
