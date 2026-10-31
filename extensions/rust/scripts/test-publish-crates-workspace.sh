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
        if [[ -f "${CARGO_STATE}/app-published" ]]; then
          exit 0
        fi
        count_file="${CARGO_STATE}/app-info-count"
        count=0
        if [[ -f "${count_file}" ]]; then
          count="$(<"${count_file}")"
        fi
        count=$((count + 1))
        printf '%s' "${count}" > "${count_file}"
        if [[ "${CARGO_SCENARIO:-workspace}" == "workspace" && "${count}" -ge 3 ]]; then
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
    count_file="${CARGO_STATE}/app-publish-count"
    count=0
    [[ -f "${count_file}" ]] && count="$(<"${count_file}")"
    count=$((count + 1))
    printf '%s' "${count}" > "${count_file}"
    case "${CARGO_SCENARIO:-workspace}" in
      rate-success)
        if [[ "${count}" -eq 1 ]]; then
          printf 'failed to publish to https://crates.io: HTTP 429 Too Many Requests. Please try again after Wed, 15 Jul 2026 07:28:56 GMT\n' >&2
          exit 1
        fi
        ;;
      rate-exhaust)
        printf 'failed to publish to https://crates.io: HTTP 429 Too Many Requests. Please try again after Wed, 15 Jul 2026 07:28:56 GMT\n' >&2
        exit 1
        ;;
      hard-failure)
        printf 'publish failed\n' >&2
        exit 1
        ;;
    esac
    if [[ "${CARGO_SCENARIO:-workspace}" != "workspace" ]]; then
      touch "${CARGO_STATE}/app-published"
    fi
    ;;
  *)
    printf 'unexpected cargo command: %s\n' "$*" >&2
    exit 1
    ;;
esac
SH
chmod +x "${BIN_DIR}/cargo"

cat > "${BIN_DIR}/date" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == *'-j -f'* ]]; then
  printf '110\n'
else
  printf '100\n'
fi
SH
chmod +x "${BIN_DIR}/date"

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

rm -f "${CARGO_STATE}"/* "${CARGO_LOG}"
CARGO_SCENARIO=rate-success HOMEBOY_CRATES_IO_PUBLISH_RETRY_SAFETY_MARGIN_SECONDS=5 bash "${SCRIPT_DIR}/publish-crates.sh" >"${TMP_DIR}/rate-success.out" 2>"${TMP_DIR}/rate-success.err"
if ! grep -qx 'sleep 15' "${CARGO_LOG}" || ! grep -q 'HTTP 429 Too Many Requests' "${TMP_DIR}/rate-success.err"; then
  printf 'rate-limit retry did not retain output and wait for Retry-After\n' >&2
  exit 1
fi

rm -f "${CARGO_STATE}"/* "${CARGO_LOG}"
if CARGO_SCENARIO=rate-exhaust HOMEBOY_CRATES_IO_PUBLISH_ATTEMPTS=2 bash "${SCRIPT_DIR}/publish-crates.sh" >/dev/null 2>&1; then
  printf 'rate-limit retry exhaustion did not fail the release script\n' >&2
  exit 1
fi
if [[ "$(grep -c '^publish ' "${CARGO_LOG}")" -ne 2 ]] || [[ "$(grep -c '^sleep ' "${CARGO_LOG}")" -ne 1 ]]; then
  printf 'rate-limit retry exhaustion did not honor the attempt limit\n' >&2
  exit 1
fi

rm -f "${CARGO_STATE}"/* "${CARGO_LOG}"
if CARGO_SCENARIO=hard-failure HOMEBOY_CRATES_IO_PUBLISH_ATTEMPTS=3 bash "${SCRIPT_DIR}/publish-crates.sh" >/dev/null 2>&1; then
  printf 'non-rate-limit publish failure did not fail the release script\n' >&2
  exit 1
fi
if [[ "$(grep -c '^publish ' "${CARGO_LOG}")" -ne 1 ]] || grep -q '^sleep ' "${CARGO_LOG}"; then
  printf 'non-rate-limit publish failure retried\n' >&2
  exit 1
fi

printf 'publish-crates workspace test passed\n'
