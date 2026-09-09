#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" != "--github" ]; then
  echo "usage: $0 --github" >&2
  exit 2
fi

config="${REQUIRED_GATES_CONFIG:-.github/required-gates-ruleset.json}"
ruleset_id="${GH_RULESET_ID:-13680120}"
repository="${GITHUB_REPOSITORY:-Extra-Chill/homeboy}"
branch="${GH_TARGET_BRANCH:-main}"
head_sha="${REQUIRED_GATES_HEAD_SHA:-${GITHUB_SHA:-unknown}}"

if [ ! -f "${config}" ]; then
  echo "required-gates ruleset candidate not found: ${config}" >&2
  exit 1
fi

live="${REQUIRED_GATES_LIVE_RULESET:-}"
if [ -n "${live}" ]; then
  if [ ! -f "${live}" ]; then
    echo "required-gates live ruleset fixture not found: ${live}" >&2
    exit 1
  fi
  payload="$(<"${live}")"
else
  if ! command -v gh >/dev/null 2>&1; then
    echo "gh is required to query the live ruleset" >&2
    exit 1
  fi
  payload="$(gh api "repos/${repository}/rulesets/${ruleset_id}")"
fi

project_contract() {
  jq -cS '{name, target, enforcement, bypass_actors: (.bypass_actors // []), conditions, rules}'
}

expected_contract="$(project_contract < "${config}")"
live_contract="$(project_contract <<<"${payload}")"
expected_contexts="$(jq -c '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context] | sort' <<<"${expected_contract}")"
live_contexts="$(jq -c '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context] | sort' <<<"${live_contract}")"

outcome="enforced"
reason=""
if [ "${live_contexts}" = '[]' ]; then
  outcome="absent"
  reason="required_status_checks is absent"
elif [ "${expected_contract}" != "${live_contract}" ]; then
  outcome="divergent"
  reason="live ruleset differs from the declared candidate"
fi

echo "required-gates-ruleset repo=${repository} branch=${branch} ruleset=${ruleset_id} head=${head_sha} outcome=${outcome} expected_contexts=${expected_contexts} live_contexts=${live_contexts} expected_contract=${expected_contract} live_contract=${live_contract}"

if [ "${outcome}" != "enforced" ]; then
  echo "::error::required-gates-ruleset: ${reason}" >&2
  exit 1
fi
