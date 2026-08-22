#!/usr/bin/env bash
# Audit or converge the one repository-owned main ruleset to its versioned policy.
set -euo pipefail

repo='Extra-Chill/homeboy'
ruleset_id='13680120'
config='.github/required-gates-ruleset.json'
operation=''
evidence=''
gh_bin="${GH_BIN:-gh}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --operation) operation="$2"; shift 2 ;;
    --evidence) evidence="$2"; shift 2 ;;
    *) echo "usage: $0 --operation <audit|reconcile> --evidence <path>" >&2; exit 2 ;;
  esac
done

if [ "${operation}" != 'audit' ] && [ "${operation}" != 'reconcile' ]; then
  echo "operation must be audit or reconcile" >&2
  exit 2
fi

if [ ! -f "${config}" ] || [ -z "${evidence}" ]; then
  echo "the versioned ruleset policy and an evidence path are required" >&2
  exit 2
fi

project_ruleset() {
  jq -S '{name, target, enforcement, bypass_actors: (.bypass_actors // []), conditions, rules}'
}

desired="$(project_ruleset < "${config}")"
before="$(${gh_bin} api "repos/${repo}/rulesets/${ruleset_id}")"
before_contract="$(project_ruleset <<< "${before}")"
changed=false

if [ "${before_contract}" != "${desired}" ] && [ "${operation}" = 'reconcile' ]; then
  payload="$(mktemp)"
  trap 'rm -f "${payload}"' EXIT
  printf '%s\n' "${desired}" > "${payload}"
  ${gh_bin} api --method PUT "repos/${repo}/rulesets/${ruleset_id}" --input "${payload}" >/dev/null
  changed=true
fi

after="$(${gh_bin} api "repos/${repo}/rulesets/${ruleset_id}")"
after_contract="$(project_ruleset <<< "${after}")"
matched=false
if [ "${after_contract}" = "${desired}" ]; then
  matched=true
fi

jq -n \
  --arg repository "${repo}" \
  --arg ruleset_id "${ruleset_id}" \
  --arg operation "${operation}" \
  --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson before "${before}" \
  --argjson desired "${desired}" \
  --argjson after "${after}" \
  --argjson changed "${changed}" \
  --argjson matched "${matched}" \
  '{repository: $repository, ruleset_id: $ruleset_id, operation: $operation, timestamp: $timestamp, changed: $changed, matched: $matched, before: $before, desired: $desired, after: $after}' \
  > "${evidence}"

if [ "${matched}" != true ]; then
  echo "ruleset ${ruleset_id} drifted from ${config}; evidence: ${evidence}" >&2
  exit 1
fi

echo "required-gates-ruleset operation=${operation} ruleset=${ruleset_id} changed=${changed} matched=${matched} evidence=${evidence}"
