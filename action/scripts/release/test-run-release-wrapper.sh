#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUN_RELEASE="${ROOT_DIR}/scripts/release/run-release.sh"

assert_contains() {
  local needle="$1"
  local haystack="$2"
  local label="$3"

  if [[ "${haystack}" != *"${needle}"* ]]; then
    printf 'FAIL: %s\nmissing: %s\nin: %s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_not_contains() {
  local needle="$1"
  local haystack="$2"
  local label="$3"

  if [[ "${haystack}" == *"${needle}"* ]]; then
    printf 'FAIL: %s\nforbidden: %s\nin: %s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_output_line() {
  local expected="$1"
  local file_path="$2"
  local label="$3"

  if ! grep -Fxq "${expected}" "${file_path}"; then
    printf 'FAIL: %s\nmissing output: %s\nactual:\n' "${label}" "${expected}"
    cat "${file_path}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

setup_fixture() {
  TEST_DIR="$(mktemp -d)"
  BIN_DIR="${TEST_DIR}/bin"
  WORKSPACE="${TEST_DIR}/workspace"
  OUTPUT_FILE="${TEST_DIR}/github-output"
  HOMEBOY_ARGS_FILE="${TEST_DIR}/homeboy-args"
  HOMEBOY_AUTH_FILE="${TEST_DIR}/homeboy-auth"
  MOCK_GIT_LOG="${TEST_DIR}/git-log"
  mkdir -p "${BIN_DIR}" "${WORKSPACE}"

  cat > "${WORKSPACE}/homeboy.json" <<'JSON'
{"id":"mock-component"}
JSON

  cat > "${BIN_DIR}/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
# Track every git invocation so the test can assert what the wrapper does
# (and just as importantly, what it no longer does — see homeboy core PR
# #2368, which moved tip-sync into `homeboy release` and removed the
# wrapper's redundant `git pull --ff-only`).
if [ -n "${MOCK_GIT_LOG:-}" ]; then
  printf '%s\n' "$*" >> "${MOCK_GIT_LOG}"
fi
case "$1 $2" in
  "rev-parse --abbrev-ref")
    echo "${MOCK_BRANCH:-main}"
    ;;
  "symbolic-ref --short")
    echo "origin/${MOCK_DEFAULT_BRANCH:-main}"
    ;;
  *)
    echo "unexpected git args: $*" >&2
    exit 2
    ;;
esac
SH

  cat > "${BIN_DIR}/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [ "$1" != "--output" ]; then
  echo "missing --output" >&2
  exit 2
fi

output_file="$2"
shift 2
if [ "$1" != "release" ]; then
  echo "expected release command" >&2
  exit 2
fi
shift

printf '%s\n' "$*" > "${HOMEBOY_ARGS_FILE}"

if [ -n "${GH_TOKEN:-}" ]; then
  if [ -z "${GIT_ASKPASS:-}" ] || [ ! -x "${GIT_ASKPASS}" ]; then
    echo "missing executable GIT_ASKPASS" >&2
    exit 2
  fi
  {
    "${GIT_ASKPASS}" 'Username for https://github.com'
    "${GIT_ASKPASS}" 'Password for https://x-access-token@github.com'
    printf '%s\n' "${GIT_TERMINAL_PROMPT:-}"
  } > "${HOMEBOY_AUTH_FILE}"
fi

case "${HOMEBOY_MOCK_SCENARIO}" in
  released)
    cat > "${output_file}" <<'JSON'
{"success":true,"data":{"command":"release","result":{"component_id":"mock-component","new_version":"2.1.0","tag":"v2.1.0","bump_type":"minor","releasable_commits":3}}}
JSON
    ;;
  skipped)
    cat > "${output_file}" <<'JSON'
{"success":true,"data":{"command":"release","result":{"component_id":"mock-component","skipped_reason":"no-releasable-commits","bump_type":"patch"}}}
JSON
    ;;
  failed)
    cat > "${output_file}" <<'JSON'
{"success":false,"error":{"message":"mock failure"}}
JSON
    exit 1
    ;;
  *)
    echo "unknown HOMEBOY_MOCK_SCENARIO=${HOMEBOY_MOCK_SCENARIO}" >&2
    exit 2
    ;;
esac
SH

  cat > "${BIN_DIR}/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$*" = "release view v2.1.0 --repo Extra-Chill/homeboy-action" ]; then
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 2
SH

  chmod +x "${BIN_DIR}/git" "${BIN_DIR}/homeboy" "${BIN_DIR}/gh"
}

run_wrapper() {
  PATH="${BIN_DIR}:${PATH}" \
  GITHUB_OUTPUT="${OUTPUT_FILE}" \
  GITHUB_WORKSPACE="${WORKSPACE}" \
  GITHUB_REPOSITORY="Extra-Chill/homeboy-action" \
  HOMEBOY_ARGS_FILE="${HOMEBOY_ARGS_FILE}" \
  HOMEBOY_AUTH_FILE="${HOMEBOY_AUTH_FILE}" \
  HOMEBOY_MOCK_SCENARIO="${HOMEBOY_MOCK_SCENARIO}" \
  MOCK_GIT_LOG="${MOCK_GIT_LOG}" \
  RELEASE_DRY_RUN="${RELEASE_DRY_RUN:-false}" \
  RELEASE_SKIP_PUBLISH="${RELEASE_SKIP_PUBLISH:-false}" \
  RELEASE_SKIP_GITHUB_RELEASE="${RELEASE_SKIP_GITHUB_RELEASE:-false}" \
  GH_TOKEN="${GH_TOKEN:-}" \
  bash "${RUN_RELEASE}"
}

assert_no_git_pull() {
  local label="$1"
  if grep -q '^pull ' "${MOCK_GIT_LOG}"; then
    printf 'FAIL: %s\nwrapper invoked git pull (now owned by homeboy core):\n' "${label}"
    cat "${MOCK_GIT_LOG}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains 'mock-component' "${ARGS}" "release passes component id"
assert_contains '--path' "${ARGS}" "release passes component path"
assert_contains '--skip-checks' "${ARGS}" "release skips duplicate checks"
assert_not_contains '--skip-publish' "${ARGS}" "release runs package/publish steps by default"
assert_not_contains '--no-github-release' "${ARGS}" "release creates GitHub Releases by default"
assert_contains '--git-identity bot' "${ARGS}" "release delegates bot identity to homeboy"
assert_not_contains '--dry-run' "${ARGS}" "normal release uses one non-dry-run homeboy invocation"
assert_output_line 'x-access-token' "${HOMEBOY_AUTH_FILE}" "release exposes GitHub token username through askpass"
assert_output_line 'secret123' "${HOMEBOY_AUTH_FILE}" "release exposes GitHub token password through askpass"
assert_output_line '0' "${HOMEBOY_AUTH_FILE}" "release disables interactive git prompts"
assert_output_line 'released=true' "${OUTPUT_FILE}" "released output is true"
assert_output_line 'release-version=2.1.0' "${OUTPUT_FILE}" "release version output is translated"
assert_output_line 'release-tag=v2.1.0' "${OUTPUT_FILE}" "release tag output is translated"
assert_output_line 'release-bump-type=minor' "${OUTPUT_FILE}" "release bump type output is translated"
assert_output_line 'bump-type=minor' "${OUTPUT_FILE}" "legacy step bump type output is preserved"
assert_no_git_pull "wrapper does not run its own git pull (core owns tip-sync)"

setup_fixture
HOMEBOY_MOCK_SCENARIO="skipped"
GH_TOKEN="secret123"
run_wrapper
assert_output_line 'released=false' "${OUTPUT_FILE}" "skipped release output is false"
assert_output_line 'skipped-reason=no-releasable-commits' "${OUTPUT_FILE}" "skipped reason comes from homeboy"
assert_output_line 'release-bump-type=patch' "${OUTPUT_FILE}" "skipped release preserves bump type when present"

setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
RELEASE_SKIP_PUBLISH="true"
RELEASE_SKIP_GITHUB_RELEASE="true"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains '--skip-publish' "${ARGS}" "release can opt out of package/publish steps"
assert_contains '--no-github-release' "${ARGS}" "release can opt out of GitHub Release creation"
unset RELEASE_SKIP_PUBLISH RELEASE_SKIP_GITHUB_RELEASE

setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
RELEASE_DRY_RUN="true"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains '--dry-run' "${ARGS}" "dry-run mode passes through to homeboy"
assert_output_line 'released=false' "${OUTPUT_FILE}" "dry-run output is not released"
assert_output_line 'release-version=2.1.0' "${OUTPUT_FILE}" "dry-run preserves planned version"
assert_output_line 'release-tag=v2.1.0' "${OUTPUT_FILE}" "dry-run preserves planned tag"
assert_output_line 'release-bump-type=minor' "${OUTPUT_FILE}" "dry-run preserves planned bump"
assert_output_line 'skipped-reason=dry-run' "${OUTPUT_FILE}" "dry-run reason is action glue"

printf 'All run-release wrapper checks passed.\n'
