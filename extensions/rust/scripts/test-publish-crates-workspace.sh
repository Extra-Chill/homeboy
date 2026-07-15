#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

BIN_DIR="${TMP_DIR}/bin"
mkdir -p "${BIN_DIR}"

cat > "${BIN_DIR}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

case "$1" in
  metadata)
    cat <<'JSON'
{"workspace_members":["app 1.0.0","contract 1.0.0","private 1.0.0"],"packages":[{"id":"app 1.0.0","name":"app","version":"1.0.0","publish":null},{"id":"contract 1.0.0","name":"contract","version":"1.0.0","publish":null},{"id":"private 1.0.0","name":"private","version":"1.0.0","publish":[]}],"resolve":{"nodes":[{"id":"app 1.0.0","deps":[{"pkg":"contract 1.0.0"}]},{"id":"contract 1.0.0","deps":[]},{"id":"private 1.0.0","deps":[]}]}}
JSON
    ;;
  info)
    identity="$2"
    printf 'info %s\n' "${identity}" >> "${CARGO_LOG}"
    case "${identity}" in
      contract@1.0.0)
        exit 0
        ;;
      app@1.0.0)
        count_file="${CARGO_STATE}/app-info-count"
        count=0
        if [[ -f "${count_file}" ]]; then
          count="$(<"${count_file}")"
        fi
        count=$((count + 1))
        printf '%s' "${count}" > "${count_file}"
        if [[ "${count}" -ge 3 ]]; then
          exit 0
        fi
        exit 1
        ;;
    esac
    exit 1
    ;;
  publish)
    printf 'publish %s\n' "$*" >> "${CARGO_LOG}"
    [[ "$*" == *'--package app'* ]]
    ;;
  *)
    printf 'unexpected cargo command: %s\n' "$*" >&2
    exit 1
    ;;
esac
SH
chmod +x "${BIN_DIR}/cargo"

cat > "${BIN_DIR}/sleep" <<'SH'
#!/usr/bin/env bash
printf 'sleep %s\n' "$*" >> "${CARGO_LOG}"
SH
chmod +x "${BIN_DIR}/sleep"

export PATH="${BIN_DIR}:${PATH}"
export CARGO_LOG="${TMP_DIR}/cargo.log"
export CARGO_STATE="${TMP_DIR}/state"
mkdir -p "${CARGO_STATE}"

bash "${SCRIPT_DIR}/publish-crates.sh" >/dev/null

expected=$'info contract@1.0.0\ninfo app@1.0.0\npublish publish --package app --locked --allow-dirty\ninfo app@1.0.0\nsleep 5\ninfo app@1.0.0'
actual="$(<"${CARGO_LOG}")"
if [[ "${actual}" != "${expected}" ]]; then
  printf 'unexpected workspace publish sequence:\n%s\n' "${actual}" >&2
  exit 1
fi

if grep -q 'private' "${CARGO_LOG}"; then
  printf 'publish=false package was included in the release sequence\n' >&2
  exit 1
fi

FAIL_BIN_DIR="${TMP_DIR}/failure-bin"
mkdir -p "${FAIL_BIN_DIR}"
cat > "${FAIL_BIN_DIR}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  metadata) printf '{"workspace_members":["broken 1.0.0"],"packages":[{"id":"broken 1.0.0","name":"broken","version":"1.0.0","publish":null}],"resolve":{"nodes":[{"id":"broken 1.0.0","deps":[]}]}}' ;;
  info) exit 1 ;;
  publish) printf 'publish failed\n' >&2; exit 1 ;;
esac
SH
chmod +x "${FAIL_BIN_DIR}/cargo"

if PATH="${FAIL_BIN_DIR}:${PATH}" bash "${SCRIPT_DIR}/publish-crates.sh" >/dev/null 2>&1; then
  printf 'publish failure did not fail the release script\n' >&2
  exit 1
fi

printf 'publish-crates workspace test passed\n'
