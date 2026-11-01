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
# Simulate a source that cannot serve the install (e.g. a ref missing there).
if [ -n "${HOMEBOY_FAIL_SOURCE:-}" ] && [[ " $* " == *" ${HOMEBOY_FAIL_SOURCE} "* ]]; then
  exit 1
fi
STUB
chmod +x "${TMP_DIR}/bin/homeboy"

export PATH="${TMP_DIR}/bin:${PATH}"
export EXTENSION_SOURCE="/extensions"
export EXTENSION_REF=""
export EXTENSION_ID="wordpress"
export COMPONENT_DIR="${TMP_DIR}/component"
export HOMEBOY_CALL_LOG="${TMP_DIR}/calls.log"

mkdir -p "${COMPONENT_DIR}"
cat > "${COMPONENT_DIR}/homeboy.json" <<'JSON'
{
  "id": "demo",
  "extensions": {
    "wordpress": {},
    "nodejs": {}
  }
}
JSON

EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source /extensions' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "all configured extensions refresh cached copy before install-for-component"

: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_INSTALL_FOR_COMPONENT_HELP_STATUS="1" EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall wordpress\nextension install /extensions --id wordpress' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "older homeboy refreshes cached copy before fallback install"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_INPUT="nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "explicit extension input refreshes cached copy before install"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_REF="aeb1bab" EXTENSION_INPUT="nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aeb1bab' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "explicit extension input installs the requested revision"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_REF="aeb1bab" EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aeb1bab\nextension uninstall wordpress\nextension install /extensions --id wordpress --ref aeb1bab' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "configured extensions install the requested revision"

: > "${HOMEBOY_CALL_LOG}"
if EXTENSION_INPUT="wordpress,nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>"${TMP_DIR}/comma.err"; then
  echo "FAIL: comma-separated extension input should fail"
  exit 1
fi
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "comma-separated input does not invoke homeboy"

MONO="https://github.com/Extra-Chill/homeboy"
LEGACY="https://github.com/Extra-Chill/homeboy-extensions"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_SOURCE="${MONO}" EXTENSION_REF="wordpress-v3.53.12" EXTENSION_INPUT="wordpress" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall wordpress\nextension install '"${MONO}"$' --id wordpress --ref wordpress-v3.53.12' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "monorepo source installs without touching the frozen repository"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_SOURCE="${MONO}" EXTENSION_REF="dd63b7be" EXTENSION_INPUT="" HOMEBOY_FAIL_SOURCE="${MONO}" \
  bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >"${TMP_DIR}/fallback.out" 2>&1
assert_equals \
  $'extension uninstall nodejs\nextension install '"${MONO}"$' --id nodejs --ref dd63b7be\nextension uninstall nodejs\nextension install '"${LEGACY}"$' --id nodejs --ref dd63b7be\nextension uninstall wordpress\nextension install '"${LEGACY}"$' --id wordpress --ref dd63b7be' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "a failed monorepo install stops at the first failure and retries every extension from the frozen repository"
assert_equals "1" "$(grep -c '^::warning::.*retrying from the frozen' "${TMP_DIR}/fallback.out")" "fallback emits one warning"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_SOURCE="${MONO}.git" EXTENSION_INPUT="" HOMEBOY_FAIL_SOURCE="${MONO}.git" \
  bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>&1
assert_equals \
  $'extension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source '"${MONO}"$'.git\nextension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source '"${LEGACY}" \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "install-for-component falls back too, and a .git suffix still counts as the monorepo"

: > "${HOMEBOY_CALL_LOG}"
if EXTENSION_SOURCE="/custom" EXTENSION_INPUT="wordpress" HOMEBOY_FAIL_SOURCE="/custom" \
  bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>&1; then
  echo "FAIL: a failing custom source should fail without a fallback"
  exit 1
fi
assert_equals \
  $'extension uninstall wordpress\nextension install /custom --id wordpress' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "a custom extension-source never falls back"

echo "All install-extension checks passed."
