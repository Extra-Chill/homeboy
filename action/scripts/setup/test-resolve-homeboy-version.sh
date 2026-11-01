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

assert_contains() {
  local needle="$1"
  local file="$2"
  local label="$3"

  if ! grep -qF "${needle}" "${file}"; then
    printf 'FAIL: %s\nmissing: %s\n' "${label}" "${needle}"
    printf 'log:\n'
    cat "${file}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_not_contains() {
  local needle="$1"
  local file="$2"
  local label="$3"

  if grep -qF "${needle}" "${file}"; then
    printf 'FAIL: %s\nunexpectedly found: %s\n' "${label}" "${needle}"
    printf 'log:\n'
    cat "${file}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_resolver_succeeds() {
  local status="$1"
  local log_file="$2"
  local label="$3"

  if [ "${status}" -ne 0 ]; then
    printf 'FAIL: %s\n' "${label}"
    printf 'log:\n'
    cat "${log_file}"
    exit 1
  fi
}

TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

FAKE_BIN="${TMPDIR}/bin"
mkdir -p "${FAKE_BIN}"

# The fake emulates the output gh --jq produces, not raw API JSON. An empty
# printf therefore stands in for the issue's HTTP 200 `[]` listing body, which
# gh --jq renders as no output at all.
cat > "${FAKE_BIN}/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [ -n "${FAKE_GH_FAILURE:-}" ]; then
  printf '%s\n' "${FAKE_GH_FAILURE}" >&2
  exit 1
fi

endpoint=""
for arg in "$@"; do
  case "${arg}" in
    repos/*) endpoint="${arg}" ;;
  esac
done

read_count() {
  local file="$1"
  if [ -f "${file}" ]; then
    cat "${file}"
  else
    printf '0'
  fi
}

log_request() {
  printf '%s\n' "${endpoint}" >> "${FAKE_STATE_DIR}/requests.log"
}

case "${endpoint}" in
  repos/Extra-Chill/homeboy/releases/latest)
    log_request
    printf '%b' "${FAKE_LATEST_ROWS:-}"
    ;;
  repos/Extra-Chill/homeboy/releases?per_page=100)
    log_request
    if [ "${FAKE_LIST_EMPTY_FIRST:-0}" = "1" ]; then
      count_file="${FAKE_STATE_DIR}/listing.attempts"
      count="$(read_count "${count_file}")"
      count=$((count + 1))
      printf '%s' "${count}" > "${count_file}"
      if [ "${count}" -le 1 ]; then
        exit 0
      fi
    fi
    printf '%b' "${FAKE_RELEASE_ROWS:-}"
    ;;
  repos/Extra-Chill/homeboy/releases/tags/*)
    tag="${endpoint##*/}"
    attempts_file="${FAKE_STATE_DIR}/${tag}.attempts"
    count="$(read_count "${attempts_file}")"
    count=$((count + 1))
    printf '%s' "${count}" > "${attempts_file}"
    log_request

    assets_var="FAKE_ASSETS_${tag//[^A-Za-z0-9_]/_}"
    ready_after_var="FAKE_TAGS_READY_AFTER_${tag//[^A-Za-z0-9_]/_}"
    ready_after="${!ready_after_var:-0}"
    if [ "${count}" -lt "${ready_after}" ]; then
      exit 0
    fi
    assets="${!assets_var:-}"
    for asset in ${assets}; do
      printf '%s\n' "${asset}"
    done
    ;;
  *)
    printf 'unexpected gh endpoint: %s\n' "${endpoint}" >&2
    exit 1
    ;;
esac
SH
chmod +x "${FAKE_BIN}/gh"

run_resolver() {
  local version="$1"
  local output_file="$2"
  local log_file="$3"
  local attempts="${4:-2}"

  PATH="${FAKE_BIN}:${PATH}" \
  GITHUB_OUTPUT="${output_file}" \
  HOMEBOY_VERSION="${version}" \
  HOMEBOY_ASSET_RETRY_ATTEMPTS="${attempts}" \
  HOMEBOY_ASSET_RETRY_DELAY="0" \
  bash "${SCRIPT_DIR}/resolve-homeboy-version.sh" > "${log_file}" 2>&1
}

case "$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)" in
  linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
  linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
  darwin-arm64) TARGET="aarch64-apple-darwin" ;;
  *)
    printf 'Unsupported test platform: %s-%s\n' "$(uname -s)" "$(uname -m)" >&2
    exit 1
    ;;
esac
ARCHIVE="homeboy-${TARGET}.tar.xz"
TAB=$'\t'

GITHUB_OUTPUT_FILE="${TMPDIR}/output-complete"
LOG_FILE="${TMPDIR}/log-complete"
mkdir -p "${TMPDIR}/state-complete"
status=0
FAKE_STATE_DIR="${TMPDIR}/state-complete" \
FAKE_LATEST_ROWS="v0.122.0${TAB}${ARCHIVE}"$'\n' \
run_resolver "latest" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}" || status=$?
assert_resolver_succeeds "${status}" "${LOG_FILE}" "latest should resolve from the releases/latest endpoint"
assert_equals "resolved-version=0.122.0" "$(cat "${GITHUB_OUTPUT_FILE}")" "latest prefers the releases/latest endpoint when it carries the asset"

GITHUB_OUTPUT_FILE="${TMPDIR}/output-scan"
LOG_FILE="${TMPDIR}/log-scan"
mkdir -p "${TMPDIR}/state-scan"
status=0
FAKE_STATE_DIR="${TMPDIR}/state-scan" \
FAKE_LATEST_ROWS="v0.122.0${TAB}other-platform.tar.xz"$'\n' \
FAKE_RELEASE_ROWS="v0.122.0${TAB}other-platform.tar.xz"$'\n'"v0.121.0${TAB}other-platform.tar.xz"$'\n'"v0.120.0${TAB}${ARCHIVE}"$'\n'"v0.119.0${TAB}${ARCHIVE}"$'\n' \
run_resolver "latest" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}" || status=$?
assert_resolver_succeeds "${status}" "${LOG_FILE}" "latest should fall back to the paged scan"
assert_equals "resolved-version=0.120.0" "$(cat "${GITHUB_OUTPUT_FILE}")" "latest scans newest-to-oldest for newest asset-bearing release"

GITHUB_OUTPUT_FILE="${TMPDIR}/output-missing"
LOG_FILE="${TMPDIR}/log-missing"
mkdir -p "${TMPDIR}/state-missing"
if FAKE_STATE_DIR="${TMPDIR}/state-missing" \
  FAKE_LATEST_ROWS="v0.122.0${TAB}other-platform.tar.xz"$'\n' \
  FAKE_RELEASE_ROWS="v0.122.0${TAB}other-platform.tar.xz"$'\n'"v0.121.0${TAB}another-platform.tar.xz"$'\n' \
  run_resolver "latest" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}"; then
  printf 'FAIL: latest without a platform asset should fail\n'
  exit 1
fi
assert_contains "Could not find a stable Homeboy release with asset ${ARCHIVE}" "${LOG_FILE}" "latest missing asset fails clearly"

GITHUB_OUTPUT_FILE="${TMPDIR}/output-pinned"
LOG_FILE="${TMPDIR}/log-pinned"
mkdir -p "${TMPDIR}/state-pinned"
if FAKE_STATE_DIR="${TMPDIR}/state-pinned" \
  FAKE_ASSETS_v0_122_0="" \
  FAKE_RELEASE_ROWS="v0.121.0${TAB}${ARCHIVE}"$'\n' \
  run_resolver "0.122.0" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}"; then
  printf 'FAIL: pinned missing asset should fail\n'
  exit 1
fi
assert_contains "explicit versions do not fall back" "${LOG_FILE}" "pinned missing asset fails clearly"
assert_contains 'select(.draft == false and .prerelease == false)' "${SCRIPT_DIR}/resolve-homeboy-version.sh" "latest scans only stable releases"

# Regression for Extra-Chill/homeboy-action#550: the releases API intermittently
# answers HTTP 200 with the body `[]`, which the fake renders as an empty
# response. Both lookups must retry inside the existing budget instead of
# concluding on the first empty listing, so the resolver still succeeds.
GITHUB_OUTPUT_FILE="${TMPDIR}/output-empty-first"
LOG_FILE="${TMPDIR}/log-empty-first"
mkdir -p "${TMPDIR}/state-empty-first"
status=0
FAKE_STATE_DIR="${TMPDIR}/state-empty-first" \
FAKE_LATEST_ROWS="" \
FAKE_LIST_EMPTY_FIRST="1" \
FAKE_RELEASE_ROWS="v0.121.0${TAB}${ARCHIVE}"$'\n' \
run_resolver "latest" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}" || status=$?
assert_resolver_succeeds "${status}" "${LOG_FILE}" "latest should retry empty responses and still resolve"
assert_equals "resolved-version=0.121.0" "$(cat "${GITHUB_OUTPUT_FILE}")" "latest retries empty responses and then resolves"

GITHUB_OUTPUT_FILE="${TMPDIR}/output-pinned-retry"
LOG_FILE="${TMPDIR}/log-pinned-retry"
mkdir -p "${TMPDIR}/state-pinned-retry"
status=0
FAKE_STATE_DIR="${TMPDIR}/state-pinned-retry" \
FAKE_ASSETS_v0_122_0="${ARCHIVE}" \
FAKE_TAGS_READY_AFTER_v0_122_0="3" \
run_resolver "0.122.0" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}" 3 || status=$?
assert_resolver_succeeds "${status}" "${LOG_FILE}" "explicit version should keep retrying its tag endpoint"
assert_equals "resolved-version=0.122.0" "$(cat "${GITHUB_OUTPUT_FILE}")" "explicit version still retries only its tag endpoint"
assert_not_contains "releases/latest" "${TMPDIR}/state-pinned-retry/requests.log" "explicit versions do not consult releases/latest"
assert_not_contains "per_page=100" "${TMPDIR}/state-pinned-retry/requests.log" "explicit versions do not consult the paged listing"

# gh failures must not be discarded: the final ::error:: quotes the last gh
# stderr instead of looking identical to a missing asset.
GITHUB_OUTPUT_FILE="${TMPDIR}/output-gh-error"
LOG_FILE="${TMPDIR}/log-gh-error"
mkdir -p "${TMPDIR}/state-gh-error"
if FAKE_STATE_DIR="${TMPDIR}/state-gh-error" \
  FAKE_GH_FAILURE="gh: An HTTP error occurred: 500 Internal Server Error" \
  run_resolver "latest" "${GITHUB_OUTPUT_FILE}" "${LOG_FILE}"; then
  printf 'FAIL: latest should fail when gh keeps erroring\n'
  exit 1
fi
assert_contains "Could not find a stable Homeboy release with asset ${ARCHIVE}" "${LOG_FILE}" "repeated gh failures fail clearly"
assert_contains "last gh error: gh: An HTTP error occurred: 500 Internal Server Error" "${LOG_FILE}" "the ::error:: message quotes the last gh stderr"

printf 'All Homeboy version resolver checks passed.\n'
