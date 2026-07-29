#!/usr/bin/env bash
# Apply or inspect the versioned main-ruleset contract. The workflow supplies
# approval and credentials; this primitive keeps the mutation and evidence exact.
set -euo pipefail

repo='Extra-Chill/homeboy'
ruleset_id='13680120'
config='.github/required-gates-ruleset.json'
operation=''
mode=''
confirmation=''
emergency_issue=''
evidence=''
gh_bin="${GH_BIN:-gh}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --operation) operation="$2"; shift 2 ;;
    --dry-run) mode='dry-run'; shift ;;
    --apply) mode='apply'; shift ;;
    --confirmation) confirmation="$2"; shift 2 ;;
    --emergency-issue) emergency_issue="$2"; shift 2 ;;
    --evidence) evidence="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "${GITHUB_REPOSITORY:-${repo}}" != "${repo}" ] || [ "${GH_REPO:-${repo}}" != "${repo}" ]; then
  echo "required-gates ruleset administration is limited to ${repo}" >&2
  exit 1
fi

if [ ! -f "${config}" ] || [ -z "${operation}" ] || [ -z "${mode}" ] || [ -z "${evidence}" ]; then
  echo "usage: $0 --operation <dry-run|apply|emergency-bypass> --dry-run|--apply --evidence <path> [--confirmation <value>] [--emergency-issue <number>]" >&2
  exit 2
fi

case "${operation}" in
  dry-run|apply|emergency-bypass) ;;
  *) echo "unsupported operation: ${operation}" >&2; exit 2 ;;
esac

if [ "${operation}" = 'dry-run' ] && [ "${mode}" != 'dry-run' ]; then
  echo "the dry-run operation cannot mutate the ruleset" >&2
  exit 1
fi

if [ "${mode}" = 'apply' ]; then
  expected_confirmation='APPLY_REQUIRED_GATES'
  [ "${operation}" = 'emergency-bypass' ] && expected_confirmation='EMERGENCY_BYPASS_REQUIRED_GATES'
  if [ "${confirmation}" != "${expected_confirmation}" ]; then
    echo "refusing ${operation}: confirmation must equal ${expected_confirmation}" >&2
    exit 1
  fi
fi

before="$(${gh_bin} api "repos/${repo}/rulesets/${ruleset_id}")"
issue='null'
desired="$(cat "${config}")"

if [ "${operation}" = 'emergency-bypass' ]; then
  if ! [[ "${emergency_issue}" =~ ^[1-9][0-9]*$ ]]; then
    echo "emergency-bypass requires a positive --emergency-issue" >&2
    exit 1
  fi
  issue="$(${gh_bin} api "repos/${repo}/issues/${emergency_issue}")"
  if ! jq -e '.state == "open" and (.title | startswith("Emergency CI bypass:"))' <<<"${issue}" >/dev/null; then
    echo "emergency issue #${emergency_issue} must be open and titled 'Emergency CI bypass: ...'" >&2
    exit 1
  fi
  desired="$(jq 'del(.rules[] | select(.type == "required_status_checks"))' "${config}")"
fi

project_ruleset() {
  jq -S '{name, target, enforcement, conditions, rules}'
}

if [ "${mode}" = 'apply' ]; then
  payload="$(mktemp)"
  trap 'rm -f "${payload}"' EXIT
  printf '%s\n' "${desired}" > "${payload}"
  ${gh_bin} api --method PUT "repos/${repo}/rulesets/${ruleset_id}" --input "${payload}" >/dev/null
fi

after="$(${gh_bin} api "repos/${repo}/rulesets/${ruleset_id}")"
expected_contract="$(project_ruleset <<<"${desired}")"
actual_contract="$(project_ruleset <<<"${after}")"

jq -n \
  --arg repository "${repo}" \
  --arg ruleset_id "${ruleset_id}" \
  --arg operation "${operation}" \
  --arg mode "${mode}" \
  --arg emergency_issue "${emergency_issue}" \
  --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson before "${before}" \
  --argjson desired "${desired}" \
  --argjson after "${after}" \
  --argjson emergency_issue_record "${issue}" \
  '{repository: $repository, ruleset_id: $ruleset_id, operation: $operation, mode: $mode, emergency_issue: ($emergency_issue | select(length > 0)), timestamp: $timestamp, before: $before, desired: $desired, after: $after, emergency_issue_record: $emergency_issue_record}' \
  > "${evidence}"

if [ "${mode}" = 'apply' ] && [ "${actual_contract}" != "${expected_contract}" ]; then
  echo "live ruleset does not exactly match the requested ${operation} contract" >&2
  exit 1
fi

if [ "${operation}" = 'apply' ] && ! jq -e '
  [.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context] | length > 0
' <<<"${after}" >/dev/null; then
  echo "apply did not restore required status checks" >&2
  exit 1
fi
