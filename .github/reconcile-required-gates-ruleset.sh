#!/usr/bin/env bash
# Reconcile the existing ruleset only after its canonical main check succeeds.
set -euo pipefail

config="${REQUIRED_GATES_CONFIG:-.github/required-gates-ruleset.json}"
gh_bin="${GH_BIN:-gh}"
evidence=''
repo="${GITHUB_REPOSITORY:-}"
check_context='none'
check_name='none'
check_app_id='none'
stage='initialize'

while [ "$#" -gt 0 ]; do
  case "$1" in
    --evidence) evidence="$2"; shift 2 ;;
    *) echo "usage: $0 --evidence <path>" >&2; exit 2 ;;
  esac
done

if [ -z "${evidence}" ]; then
  echo 'an evidence path is required' >&2
  exit 2
fi

write_fallback_failure() {
  jq -n \
    --arg schema 'homeboy/required-gates-reconcile-evidence/v1' \
    --arg repository "${repo:-unknown}" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg stage "${stage}" \
    --arg check_context "${check_context}" \
    --arg check_name "${check_name}" \
    --arg check_app_id "${check_app_id}" \
    '{schema: $schema, operation: "reconcile", outcome: "failed", repository: $repository, timestamp: $timestamp, stage: $stage, reason: "command failed before detailed evidence was written", preflight: {check_context: $check_context, check_name: $check_name, check_app_id: $check_app_id}}' \
    > "${evidence}"
}

on_exit() {
  local status="$?"
  trap - EXIT
  if [ "${status}" -ne 0 ] && [ ! -s "${evidence}" ]; then
    write_fallback_failure
  fi
  exit "${status}"
}
trap on_exit EXIT

write_failure() {
  local stage="$1"
  local reason="$2"
  jq -n \
    --arg schema 'homeboy/required-gates-reconcile-evidence/v1' \
    --arg repository "${repo:-unknown}" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg stage "${stage}" \
    --arg reason "${reason}" \
    --arg check_context "${check_context}" \
    --arg check_name "${check_name}" \
    --arg check_app_id "${check_app_id}" \
    '{schema: $schema, operation: "reconcile", outcome: "failed", repository: $repository, timestamp: $timestamp, stage: $stage, reason: $reason, preflight: {check_context: $check_context, check_name: $check_name, check_app_id: $check_app_id}}' \
    > "${evidence}"
  echo "required-gates reconciliation failed at ${stage}: ${reason}; evidence: ${evidence}" >&2
  exit 1
}

stage='repository'
[ -n "${repo}" ] || write_failure 'repository' 'GITHUB_REPOSITORY is required'
stage='policy'
[ -f "${config}" ] || write_failure 'policy' 'versioned ruleset policy is missing'
stage='token'
[ -n "${GH_TOKEN:-}" ] || write_failure 'token' 'HOMEBOY_RULESET_ADMIN_TOKEN is required'

stage='policy'
declared_preflight="$(jq -r '.reconcile_preflight.required_context // empty' "${config}")"
if [ -n "${declared_preflight}" ]; then
  check_context="${declared_preflight}"
  check_name="$(jq -Rer 'sub("^[^/]+ / "; "")' <<< "${check_context}")" \
    || write_failure 'policy' 'reconcile preflight context has no check-run name'
  check_app_id="$(jq -er --arg context "${check_context}" '
    [.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]
     | select(.context == $context) | .integration_id]
    | unique
    | if length == 1 then .[0] else error("required context must have exactly one integration id") end
  ' "${config}")" || write_failure 'policy' 'reconcile preflight integration id is missing or ambiguous'
fi

if ! environment="$(${gh_bin} api "repos/${repo}/environments/main-ruleset-administration" 2>/dev/null)"; then
  write_failure 'environment' 'could not read main-ruleset-administration'
fi
jq -e '.can_admins_bypass == false and any(.protection_rules[]; .type == "required_reviewers")' \
  >/dev/null <<< "${environment}" \
  || write_failure 'environment' 'required reviewers or administrator-bypass policy is missing'

stage='main-tip'
head="$(git rev-parse HEAD)"
if ! current_main="$(${gh_bin} api "repos/${repo}/commits/main" 2>/dev/null | jq -er '.sha')"; then
  write_failure 'main-tip' 'could not read main tip'
fi
[ "${head}" = "${current_main}" ] || write_failure 'main-tip' 'checkout is not the current main tip'

if [ -n "${declared_preflight}" ]; then
  stage='check-run'
  if ! checks="$(${gh_bin} api "repos/${repo}/commits/${head}/check-runs?per_page=100" 2>/dev/null)"; then
    write_failure 'check-run' 'could not read main check runs'
  fi
  jq -e --arg name "${check_name}" --argjson app_id "${check_app_id}" '
    any(.check_runs[]; .name == $name and .app.id == $app_id and .conclusion == "success")
  ' >/dev/null <<< "${checks}" \
    || write_failure 'check-run' 'canonical successful required check is absent'
fi

stage='main-tip'
if ! current_main="$(${gh_bin} api "repos/${repo}/commits/main" 2>/dev/null | jq -er '.sha')"; then
  write_failure 'main-tip' 'could not re-read main tip'
fi
[ "${head}" = "${current_main}" ] || write_failure 'main-tip' 'main advanced during preflight'

stage='reconcile'
bash .github/manage-required-gates-ruleset.sh --operation reconcile --evidence "${evidence}"
stage='verification'
bash .github/validate-required-gates.sh --github
