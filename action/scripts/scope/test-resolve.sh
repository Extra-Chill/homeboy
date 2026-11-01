#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_SCRIPT="${SCRIPT_DIR}/resolve.sh"

# Resolve against local history only, without fetching from the project remote.
test_dir="$(mktemp -d)"
trap 'rm -rf "${test_dir}"' EXIT
git init -q "${test_dir}/repo"
cd "${test_dir}/repo"
git -c user.name=Test -c user.email=test@example.com commit -q --allow-empty -m base
base_sha="$(git rev-parse HEAD)"
git -c user.name=Test -c user.email=test@example.com commit -q --allow-empty -m candidate

assert_env_contains() {
  local file="$1"
  local expected="$2"
  local label="$3"

  if grep -qx "${expected}" "${file}"; then
    printf 'PASS: %s\n' "${label}"
    return
  fi

  printf 'FAIL: %s\nexpected env line: %s\nactual:\n' "${label}" "${expected}"
  sed 's/^/  /' "${file}"
  exit 1
}

run_resolve() {
  local env_file output_file
  env_file="$(mktemp "${test_dir}/env.XXXXXX")"
  output_file="$(mktemp "${test_dir}/output.XXXXXX")"

  GITHUB_ENV="${env_file}" \
    GITHUB_OUTPUT="${output_file}" \
    "$@" \
    bash "${RESOLVE_SCRIPT}" >/dev/null

  printf '%s\n' "${env_file}"
}

env_file="$(run_resolve env GITHUB_EVENT_NAME=push SCOPE_INPUT=auto)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "auto push uses full scope"

for event in pull_request pull_request_target; do
  env_file="$(run_resolve env GITHUB_EVENT_NAME="${event}" SCOPE_INPUT=full BASE_SHA="${base_sha}")"
  assert_env_contains "${env_file}" "SCOPE_MODE=full" "explicit full ${event} keeps full candidate scope"
  assert_env_contains "${env_file}" "SCOPE_BASE_REF=${base_sha}" "explicit full ${event} resolves baseline ref"
done

for event in push schedule workflow_dispatch; do
  env_file="$(run_resolve env GITHUB_EVENT_NAME="${event}" SCOPE_INPUT=full BASE_SHA="${base_sha}")"
  assert_env_contains "${env_file}" "SCOPE_MODE=full" "explicit full ${event} keeps full scope"
  assert_env_contains "${env_file}" "SCOPE_BASE_REF=" "explicit full ${event} has no baseline ref"
done

for base in '' missing; do
  env_file="$(run_resolve env GITHUB_EVENT_NAME=pull_request SCOPE_INPUT=full BASE_SHA="${base}")"
  assert_env_contains "${env_file}" "SCOPE_MODE=full" "full PR without resolvable base keeps full scope"
  assert_env_contains "${env_file}" "SCOPE_BASE_REF=" "full PR without resolvable base has no baseline ref"
done

env_file="$(run_resolve env GITHUB_EVENT_NAME=workflow_dispatch SCOPE_INPUT=changed)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "changed scope without base falls back to full"

env_file="$(run_resolve env GITHUB_EVENT_NAME=push SCOPE_INPUT=unexpected)"
assert_env_contains "${env_file}" "SCOPE_MODE=full" "invalid scope falls back to auto"

for scope in auto changed; do
  env_file="$(run_resolve env GITHUB_EVENT_NAME=pull_request SCOPE_INPUT="${scope}" BASE_SHA="${base_sha}" PR_HEAD_REPO=Extra-Chill/homeboy-action GITHUB_REPOSITORY=Extra-Chill/homeboy-action)"
  assert_env_contains "${env_file}" "SCOPE_MODE=changed" "${scope} PR uses changed scope"
  assert_env_contains "${env_file}" "SCOPE_BASE_REF=${base_sha}" "${scope} PR records base ref"
done

# A baseline ref must not imply changed-file behaviour in context helpers.
(
  source "${SCRIPT_DIR}/context.sh"
  export SCOPE_MODE=full SCOPE_BASE_REF="${base_sha}"
  if is_scoped || [ -n "$(scope_note_for lint)" ] || [ -n "$(scope_note_for test)" ]; then
    printf 'FAIL: full scope with baseline must not be described as changed-only\n'
    exit 1
  fi
  [ "$(scope_base_ref)" = "${base_sha}" ]
)
printf 'PASS: full scope context retains baseline without changed-only notes\n'

# --- scope_flags_for -------------------------------------------------------

assert_flags() {
  local cmd="$1" gating="$2" expected="$3" label="$4" actual
  actual="$(
    SCOPE_MODE=changed SCOPE_BASE_REF=origin/main HOMEBOY_DIFFERENTIAL_GATING="${gating}" \
    bash -c "source '${SCRIPT_DIR}/flags.sh'; scope_flags_for '${cmd}'"
  )"
  if [ "${actual}" != "${expected}" ]; then
    printf 'FAIL: %s (expected %s, got %s)\n' "${label}" "'${expected}'" "'${actual}'"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

assert_flags lint true '--changed-since origin/main' 'lint stays scoped under differential gating'
assert_flags lint false '--changed-since origin/main' 'lint is scoped without differential gating'
assert_flags review true '--changed-since origin/main' 'review stays scoped'

# Homeboy turns this one changed ref into the extension's exact changed-test
# selection. Without the CLI flag, no selection contract is produced and every
# differential Test silently expands to the full suite (homeboy#11751 W1-3).
assert_flags test true '--changed-since origin/main' 'test is scoped under differential gating'
assert_flags test false '--changed-since origin/main' 'test is scoped without differential gating'

# Never-scoped commands must stay unscoped regardless of gating.
assert_flags release true '' 'release is never scoped'
assert_flags deploy true '' 'deploy is never scoped'

# Nothing is scoped when there is no changed scope to apply.
no_scope="$(SCOPE_MODE=full SCOPE_BASE_REF=origin/main bash -c "source '${SCRIPT_DIR}/flags.sh'; scope_flags_for lint")"
[ -z "${no_scope}" ] || { printf 'FAIL: full scope must not emit --changed-since, got %s\n' "${no_scope}"; exit 1; }
missing_base="$(SCOPE_MODE=changed SCOPE_BASE_REF= bash -c "source '${SCRIPT_DIR}/flags.sh'; scope_flags_for lint")"
[ -z "${missing_base}" ] || { printf 'FAIL: absent base ref must not emit --changed-since, got %s\n' "${missing_base}"; exit 1; }
printf 'PASS: no changed scope emits no flag\n'

printf 'All scope resolver checks passed.\n'
