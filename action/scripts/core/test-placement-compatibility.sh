#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FAKE_BIN="$(mktemp -d)"
TMPDIR="$(mktemp -d)"
cleanup() {
  rm -f "${FAKE_BIN}/homeboy"
  rmdir "${FAKE_BIN}" 2>/dev/null || true
  rm -f "${TMPDIR}/github-env-current" "${TMPDIR}/github-env-legacy" "${TMPDIR}/github-env" "${TMPDIR}/github-output"
  rmdir "${TMPDIR}/homeboy-ci-results" 2>/dev/null || true
  rmdir "${TMPDIR}" 2>/dev/null || true
}
trap cleanup EXIT

assert_equals() {
  local expected="$1" actual="$2" label="$3"
  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

assert_contains() {
  local value="$1" expected="$2" label="$3"
  case "${value}" in
    *"${expected}"*) printf 'PASS: %s\n' "${label}" ;;
    *)
      printf 'FAIL: %s\nmissing: %s\nactual: %s\n' "${label}" "${expected}" "${value}"
      exit 1
      ;;
  esac
}

cat > "${FAKE_BIN}/homeboy" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "--help" ]; then
  printf 'help\n' >> "${HOMEBOY_HELP_LOG}"
  if [ "${HOMEBOY_FAKE_GENERATION}" = "current" ]; then
    printf '  --placement <auto|local|lab>\n'
  fi
  exit 0
fi

printf '%s\n' "$*" >> "${HOMEBOY_CALL_LOG}"
output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then
    output="$2"
    shift 2
    continue
  fi
  shift
done
[ -z "${output}" ] || printf '{"success":true}\n' > "${output}"
SH
chmod +x "${FAKE_BIN}/homeboy"

export PATH="${FAKE_BIN}:${PATH}"
export GITHUB_ACTIONS=true
export SCOPE_MODE=full
export SCOPE_BASE_REF=
unset HOMEBOY_SETTINGS_JSON
export HOMEBOY_HELP_LOG="${TMPDIR}/help.log"
export HOMEBOY_CALL_LOG="${TMPDIR}/calls.log"
source "${ROOT}/scripts/core/lib.sh"

test_generation() {
  local generation="$1" placement_flags="$2"
  : > "${HOMEBOY_HELP_LOG}"
  : > "${HOMEBOY_CALL_LOG}"
  export HOMEBOY_FAKE_GENERATION="${generation}"
  unset HOMEBOY_ACTION_SUPPORTS_PLACEMENT

  export GITHUB_ENV="${TMPDIR}/github-env-${generation}"
  : > "${GITHUB_ENV}"
  bash "${ROOT}/scripts/setup/detect-homeboy-placement.sh"
  IFS='=' read -r _ HOMEBOY_ACTION_SUPPORTS_PLACEMENT < "${GITHUB_ENV}"
  export HOMEBOY_ACTION_SUPPORTS_PLACEMENT

  assert_equals "homeboy ${placement_flags}--output /tmp/out.json review lint component --path /tmp/workspace" "$(build_run_command 'review lint' component /tmp/workspace /tmp/out.json)" "${generation} review builder preserves output ordering"
  assert_equals "homeboy ${placement_flags}--output /tmp/out.json bench component --path /tmp/workspace" "$(build_run_command bench component /tmp/workspace /tmp/out.json)" "${generation} bench builder"
  assert_equals "homeboy ${placement_flags}refactor component --all --path /tmp/workspace" "$(build_run_command 'refactor --all' component /tmp/workspace)" "${generation} refactor builder"
  assert_equals "homeboy ${placement_flags}refactor component --from lint --write --path /tmp/workspace" "$(build_autofix_command 'refactor --from lint --write' component /tmp/workspace)" "${generation} autofix builder"
  assert_equals "homeboy ${placement_flags}review component --path /tmp/workspace --report=pr-comment" "$(build_review_report_command component /tmp/workspace)" "${generation} report builder"
  assert_equals 1 "$(wc -l < "${HOMEBOY_HELP_LOG}" | xargs)" "${generation} capability is detected once"
}

test_generation current '--placement local '
test_generation legacy '--force-hot --allow-local-hot '

# Exercise the action execution script with a current-generation binary.
: > "${HOMEBOY_HELP_LOG}"
: > "${HOMEBOY_CALL_LOG}"
export HOMEBOY_FAKE_GENERATION=current
unset HOMEBOY_ACTION_SUPPORTS_PLACEMENT
export GITHUB_ACTION_PATH="${ROOT}"
export GITHUB_WORKSPACE="${TMPDIR}"
export GITHUB_OUTPUT="${TMPDIR}/github-output"
export GITHUB_ENV="${TMPDIR}/github-env"
export RESOLVED_COMMANDS='review audit'
export COMPONENT_NAME=component
bash "${ROOT}/scripts/setup/detect-homeboy-placement.sh"
IFS='=' read -r _ HOMEBOY_ACTION_SUPPORTS_PLACEMENT < "${GITHUB_ENV}"
export HOMEBOY_ACTION_SUPPORTS_PLACEMENT
bash "${ROOT}/scripts/core/run-homeboy-commands.sh"
assert_contains "$(< "${HOMEBOY_CALL_LOG}")" '--placement local --output ' 'current binary runs through action command execution with local placement'
assert_equals 1 "$(wc -l < "${HOMEBOY_HELP_LOG}" | xargs)" 'current execution detects placement once'

printf 'All placement compatibility checks passed.\n'
