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

# Feature-detect probe used by run-release.sh: `homeboy release --help`.
# HOMEBOY_SUPPORTS_CONFIRM_FLAG toggles whether this mock binary advertises
# the --i-know-ci-creates-the-github-release flag, emulating newer (guarded)
# vs older (pre-#6137) published homeboy release binaries.
if [ "$1" = "release" ] && [ "${2:-}" = "--help" ]; then
  echo "Usage: homeboy release [OPTIONS]"
  echo "      --no-github-release"
  if [ "${HOMEBOY_SUPPORTS_CONFIRM_FLAG:-true}" = "true" ]; then
    echo "      --i-know-ci-creates-the-github-release"
  fi
  exit 0
fi

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
  step-failed)
    # The v3 command-result envelope a release that RAN and then failed a step
    # emits: no `.error`, but a fully classified `.diagnostics` and executable
    # `.next_actions` (Extra-Chill/homeboy#10441).
    cat > "${output_file}" <<'JSON'
{"schema":"command-result/v3","command":"release","success":false,"exit_code":1,"status":"failed","summary":"Release step github.release (github.release) failed: gh-upload-failed","next_actions":[{"label":"attach the built artifacts to the release that already exists","command":"gh release upload 'v2.1.0' --clobber -R 'Extra-Chill/homeboy'","kind":"repair"}],"diagnostics":{"code":"command.failed","message":"Release step github.release (github.release) failed: gh-upload-failed — `gh release upload` failed for v2.1.0: gh api release metadata exited with status 1"},"data":{"command":"release","result":{"component_id":"mock-component"}}}
JSON
    exit 1
    ;;
  *)
    echo "unknown HOMEBOY_MOCK_SCENARIO=${HOMEBOY_MOCK_SCENARIO}" >&2
    exit 2
    ;;
esac
SH

  # MOCK_RELEASE_DRAFT_STATE drives the publish-state read the wrapper uses to
  # confirm a release actually shipped. Defaults to a published release.
  cat > "${BIN_DIR}/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$*" = "release view v2.1.0 --repo Extra-Chill/homeboy-action --json isDraft --jq .isDraft" ]; then
  printf '%s\n' "${MOCK_RELEASE_DRAFT_STATE:-false}"
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
  HOMEBOY_SUPPORTS_CONFIRM_FLAG="${HOMEBOY_SUPPORTS_CONFIRM_FLAG:-true}" \
  MOCK_RELEASE_DRAFT_STATE="${MOCK_RELEASE_DRAFT_STATE:-false}" \
  MOCK_GIT_LOG="${MOCK_GIT_LOG}" \
  RELEASE_DRY_RUN="${RELEASE_DRY_RUN:-false}" \
  RELEASE_SKIP_PUBLISH="${RELEASE_SKIP_PUBLISH:-false}" \
  RELEASE_SKIP_GITHUB_RELEASE="${RELEASE_SKIP_GITHUB_RELEASE:-false}" \
  RELEASE_HEAD="${RELEASE_HEAD:-false}" \
  RELEASE_FROM_ARTIFACTS="${RELEASE_FROM_ARTIFACTS:-}" \
  MOCK_BRANCH="${MOCK_BRANCH:-main}" \
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
assert_contains '--apply' "${ARGS}" "real release pairs --skip-checks with the required --apply opt-in"
assert_output_line 'x-access-token' "${HOMEBOY_AUTH_FILE}" "release exposes GitHub token username through askpass"
assert_output_line 'secret123' "${HOMEBOY_AUTH_FILE}" "release exposes GitHub token password through askpass"
assert_output_line '0' "${HOMEBOY_AUTH_FILE}" "release disables interactive git prompts"
assert_output_line 'released=true' "${OUTPUT_FILE}" "released output is true"
assert_output_line 'release-version=2.1.0' "${OUTPUT_FILE}" "release version output is translated"
assert_output_line 'release-tag=v2.1.0' "${OUTPUT_FILE}" "release tag output is translated"
assert_output_line 'release-bump-type=minor' "${OUTPUT_FILE}" "release bump type output is translated"
assert_output_line 'bump-type=minor' "${OUTPUT_FILE}" "legacy step bump type output is preserved"
assert_no_git_pull "wrapper does not run its own git pull (core owns tip-sync)"

# THE STRANDED-DRAFT REGRESSION (homeboy#10685).
# `homeboy release` can report success while its github.release step leaves an
# unpublished draft over the pushed tag — that is exactly how homeboy-action
# v2.8.26/v2.8.27/v2.9.0/v2.9.1 stranded on 2026-07-28 with the floating v2 tag
# frozen. A green `homeboy release` is therefore NOT proof of a shipped
# release, so the wrapper must independently assert the published state and go
# red when it finds a draft.
setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
GH_TOKEN="secret123"
MOCK_RELEASE_DRAFT_STATE="true"
DRAFT_OUTPUT="$(run_wrapper 2>&1)" && {
  printf 'FAIL: wrapper reported success over an unpublished draft release\n%s\n' "${DRAFT_OUTPUT}"
  exit 1
}
printf 'PASS: successful homeboy release over an unpublished draft exits non-zero\n'
assert_contains 'is still an unpublished DRAFT' "${DRAFT_OUTPUT}" "stranded draft is named in the CI annotation"
assert_contains 'gh release edit v2.1.0 --draft=false' "${DRAFT_OUTPUT}" "operator repair command is printed next to the stranded draft"
assert_output_line 'released=false' "${OUTPUT_FILE}" "stranded draft is not reported as released"
assert_output_line 'skipped-reason=release-not-published' "${OUTPUT_FILE}" "stranded draft reports the release-not-published reason"
unset MOCK_RELEASE_DRAFT_STATE

setup_fixture
HOMEBOY_MOCK_SCENARIO="skipped"
GH_TOKEN="secret123"
run_wrapper
assert_output_line 'released=false' "${OUTPUT_FILE}" "skipped release output is false"
assert_output_line 'skipped-reason=no-releasable-commits' "${OUTPUT_FILE}" "skipped reason comes from homeboy"
assert_output_line 'release-bump-type=patch' "${OUTPUT_FILE}" "skipped release preserves bump type when present"

# GitHub-release opt-out against a NEWER homeboy binary that advertises (and
# requires, via the #6137 guard) the confirmation flag: the wrapper must pass
# both --no-github-release and --i-know-ci-creates-the-github-release.
setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
HOMEBOY_SUPPORTS_CONFIRM_FLAG="true"
RELEASE_SKIP_PUBLISH="true"
RELEASE_SKIP_GITHUB_RELEASE="true"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains '--skip-publish' "${ARGS}" "release can opt out of package/publish steps"
assert_contains '--no-github-release' "${ARGS}" "release can opt out of GitHub Release creation"
assert_contains '--i-know-ci-creates-the-github-release' "${ARGS}" "CI opt-out confirms the flag when the binary supports it"
unset RELEASE_SKIP_PUBLISH RELEASE_SKIP_GITHUB_RELEASE HOMEBOY_SUPPORTS_CONFIRM_FLAG

# GitHub-release opt-out against an OLDER homeboy binary that PREDATES the
# confirmation flag (and the guard): the wrapper must pass --no-github-release
# alone and must NOT pass the flag, which an older binary would reject with
# "unexpected argument". This is the regression #263 introduced.
setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
HOMEBOY_SUPPORTS_CONFIRM_FLAG="false"
RELEASE_SKIP_PUBLISH="true"
RELEASE_SKIP_GITHUB_RELEASE="true"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains '--no-github-release' "${ARGS}" "older binary still opts out of GitHub Release creation"
assert_not_contains '--i-know-ci-creates-the-github-release' "${ARGS}" "older binary that lacks the flag is not handed it"
unset RELEASE_SKIP_PUBLISH RELEASE_SKIP_GITHUB_RELEASE HOMEBOY_SUPPORTS_CONFIRM_FLAG

setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
RELEASE_HEAD="true"
RELEASE_FROM_ARTIFACTS="artifacts"
MOCK_BRANCH="HEAD"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains '--head' "${ARGS}" "head release passes --head"
assert_contains '--from-artifacts' "${ARGS}" "head release passes --from-artifacts"
assert_contains 'artifacts' "${ARGS}" "head release passes artifact directory"
assert_output_line 'released=true' "${OUTPUT_FILE}" "head release can run from detached HEAD"
unset RELEASE_HEAD RELEASE_FROM_ARTIFACTS MOCK_BRANCH

setup_fixture
HOMEBOY_MOCK_SCENARIO="released"
RELEASE_DRY_RUN="true"
GH_TOKEN="secret123"
run_wrapper
ARGS="$(cat "${HOMEBOY_ARGS_FILE}")"
assert_contains '--dry-run' "${ARGS}" "dry-run mode passes through to homeboy"
assert_not_contains '--apply' "${ARGS}" "dry-run never passes --apply (mutually exclusive with --dry-run)"
assert_output_line 'released=false' "${OUTPUT_FILE}" "dry-run output is not released"
assert_output_line 'release-version=2.1.0' "${OUTPUT_FILE}" "dry-run preserves planned version"
assert_output_line 'release-tag=v2.1.0' "${OUTPUT_FILE}" "dry-run preserves planned tag"
assert_output_line 'release-bump-type=minor' "${OUTPUT_FILE}" "dry-run preserves planned bump"
assert_output_line 'skipped-reason=dry-run' "${OUTPUT_FILE}" "dry-run reason is action glue"

# Extra-Chill/homeboy#10441: a release that fails a step returns the v3
# envelope, which has no `.error`. Reading only `.error.message` printed
# "Release failed: Unknown error" while the payload held the classified reason
# AND a ready-to-paste repair command — the pipeline computed the answer and
# printed prose. The wrapper must surface both.
setup_fixture
HOMEBOY_MOCK_SCENARIO="step-failed"
GH_TOKEN="secret123"
set +e
FAILURE_OUTPUT="$(run_wrapper 2>&1)"
FAILURE_EXIT=$?
set -e
if [ "${FAILURE_EXIT}" -eq 0 ]; then
  printf 'FAIL: failed release must exit non-zero\n%s\n' "${FAILURE_OUTPUT}"
  exit 1
fi
printf 'PASS: failed release exits non-zero\n'
assert_not_contains 'Release failed: Unknown error' "${FAILURE_OUTPUT}" "classified step failure is never reported as Unknown error"
assert_contains 'gh-upload-failed' "${FAILURE_OUTPUT}" "classified failure reason reaches the CI annotation"
assert_contains "gh release upload 'v2.1.0' --clobber" "${FAILURE_OUTPUT}" "repair command is printed next to the failure"
assert_output_line 'released=false' "${OUTPUT_FILE}" "failed release reports released=false"
assert_output_line 'skipped-reason=release-failed' "${OUTPUT_FILE}" "failed release reports the release-failed reason"

# The legacy `.error.message` envelope (validation errors, load failures) must
# keep working — the new lookup adds fallbacks, it does not replace the field.
setup_fixture
HOMEBOY_MOCK_SCENARIO="failed"
GH_TOKEN="secret123"
set +e
LEGACY_OUTPUT="$(run_wrapper 2>&1)"
LEGACY_EXIT=$?
set -e
if [ "${LEGACY_EXIT}" -eq 0 ]; then
  printf 'FAIL: legacy failure must exit non-zero\n%s\n' "${LEGACY_OUTPUT}"
  exit 1
fi
printf 'PASS: legacy failure exits non-zero\n'
assert_contains 'Release failed: mock failure' "${LEGACY_OUTPUT}" "legacy .error.message envelope still reports its message"

printf 'All run-release wrapper checks passed.\n'
