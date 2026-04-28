#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin"
cat > "${TMP_DIR}/bin/homeboy" <<'STUB'
#!/usr/bin/env bash
if [ "$*" = "extension install-for-component --help" ]; then
  exit "${HOMEBOY_INSTALL_FOR_COMPONENT_HELP_STATUS:-0}"
fi
printf '%s\n' "$*" >> "${HOMEBOY_CALL_LOG}"
STUB
chmod +x "${TMP_DIR}/bin/homeboy"

export PATH="${TMP_DIR}/bin:${PATH}"
export EXTENSION_SOURCE="/extensions"
export EXTENSION_ID="wordpress"
export COMPONENT_DIR="packages/plugin"
export HOMEBOY_CALL_LOG="${TMP_DIR}/calls.log"

EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  "extension install-for-component --path packages/plugin --source /extensions" \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "configured extensions use core install-for-component"

: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_INSTALL_FOR_COMPONENT_HELP_STATUS="1" EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  "extension install /extensions --id wordpress" \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "older homeboy falls back to first configured extension"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_INPUT="nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  "extension install /extensions --id nodejs" \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "explicit extension input keeps single-extension override"

: > "${HOMEBOY_CALL_LOG}"
if EXTENSION_INPUT="wordpress,nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>"${TMP_DIR}/comma.err"; then
  echo "FAIL: comma-separated extension input should fail"
  exit 1
fi
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "comma-separated input does not invoke homeboy"

echo "All install-extension checks passed."
