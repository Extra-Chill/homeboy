#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

FAKE_BIN="${TMPDIR}/bin"
FAKE_EXTENSION="${TMPDIR}/extensions/wordpress"
mkdir -p "${FAKE_BIN}" "${FAKE_EXTENSION}"
printf '{"engines":{"node":">=18.12.0"}}\n' > "${FAKE_EXTENSION}/package.json"

cat > "${FAKE_BIN}/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [ "$1" = "component" ] && [ "$2" = "env" ]; then
  printf '{"success":true,"data":{"entity":{"php":"8.2"}}}\n'
  exit 0
fi

if [ "$1" = "extension" ] && [ "$2" = "show" ]; then
  printf '{"success":true,"data":{"extension":{"path":"%s"}}}\n' "${FAKE_EXTENSION_PATH}"
  exit 0
fi

printf 'unexpected fake homeboy invocation: %s\n' "$*" >&2
exit 1
SH
chmod +x "${FAKE_BIN}/homeboy"

GITHUB_ENV_FILE="${TMPDIR}/env"
GITHUB_OUTPUT_FILE="${TMPDIR}/output"
LOG_FILE="${TMPDIR}/log"

PATH="${FAKE_BIN}:${PATH}" \
FAKE_EXTENSION_PATH="${FAKE_EXTENSION}" \
GITHUB_ENV="${GITHUB_ENV_FILE}" \
GITHUB_OUTPUT="${GITHUB_OUTPUT_FILE}" \
COMPONENT_DIR="${TMPDIR}" \
PORTABLE_EXTENSION="wordpress" \
DEFAULT_EXTENSION_NODE_VERSION="24" \
bash "${SCRIPT_DIR}/detect-runtime-env.sh" > "${LOG_FILE}"

assert_equals "PORTABLE_PHP=8.2" "$(grep '^PORTABLE_PHP=' "${GITHUB_ENV_FILE}")" "detects component PHP"
assert_equals "PORTABLE_NODE=24" "$(grep '^PORTABLE_NODE=' "${GITHUB_ENV_FILE}")" "uses extension-required Node fallback"
assert_equals "portable-node=24" "$(grep '^portable-node=' "${GITHUB_OUTPUT_FILE}")" "writes portable-node output"

if ! grep -q 'node: 24 (required by wordpress extension setup)' "${LOG_FILE}"; then
  printf 'FAIL: extension Node requirement is explained in log\n'
  exit 1
fi
printf 'PASS: extension Node requirement is explained in log\n'

GITHUB_ENV_FILE="${TMPDIR}/env-input"
GITHUB_OUTPUT_FILE="${TMPDIR}/output-input"
PATH="${FAKE_BIN}:${PATH}" \
FAKE_EXTENSION_PATH="${FAKE_EXTENSION}" \
GITHUB_ENV="${GITHUB_ENV_FILE}" \
GITHUB_OUTPUT="${GITHUB_OUTPUT_FILE}" \
COMPONENT_DIR="${TMPDIR}" \
PORTABLE_EXTENSION="wordpress" \
NODE_INPUT="20" \
bash "${SCRIPT_DIR}/detect-runtime-env.sh" > /dev/null

assert_equals "PORTABLE_NODE=20" "$(grep '^PORTABLE_NODE=' "${GITHUB_ENV_FILE}")" "node input overrides extension fallback"
