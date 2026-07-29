#!/usr/bin/env bash
# Validate the versioned main-branch gate contract. `--github` additionally
# proves the live ruleset has the exact configured required-check set.
set -euo pipefail

mode="${1:---local}"
config=".github/required-gates-ruleset.json"
ci_workflow=".github/workflows/ci.yml"

if [ ! -f "${config}" ] || [ ! -f "${ci_workflow}" ]; then
  echo "required-gates validation must run from the repository root" >&2
  exit 1
fi

contexts=()
while IFS= read -r context; do
  contexts+=("${context}")
done < <(jq -r '
  .rules[]
  | select(.type == "required_status_checks")
  | .parameters.required_status_checks[]?.context
' "${config}")

if [ "${#contexts[@]}" -eq 0 ]; then
  echo "required-gates policy declares no required checks" >&2
  exit 1
fi

if [ "$(printf '%s\n' "${contexts[@]}" | sort -u | wc -l | tr -d ' ')" -ne "${#contexts[@]}" ]; then
  echo "required-gates policy declares duplicate check contexts" >&2
  exit 1
fi

for context in "${contexts[@]}"; do
  if grep -Fq "name: ${context}" "${ci_workflow}"; then
    continue
  fi

  title="${context#homeboy / }"
  if grep -Fq 'name: homeboy / ${{ matrix.title }}' "${ci_workflow}" \
    && grep -Fq "title: ${title}" "${ci_workflow}"; then
    continue
  fi

  {
    echo "required check '${context}' is not emitted by ${ci_workflow}" >&2
    exit 1
  }
done

if ! jq -e '
  .rules[]
  | select(.type == "required_status_checks")
  | .parameters.strict_required_status_checks_policy == true
' "${config}" >/dev/null; then
  echo "required-gates policy must require checks on the current PR head" >&2
  exit 1
fi

if [ "${mode}" = "--local" ]; then
  exit 0
fi

if [ "${mode}" != "--github" ]; then
  echo "usage: bash .github/validate-required-gates.sh [--local|--github]" >&2
  exit 2
fi

repo="${GH_REPO:-Extra-Chill/homeboy}"
ruleset_id="${GH_RULESET_ID:-13680120}"
live="$(gh api "repos/${repo}/rulesets/${ruleset_id}")"
expected="$(jq -S '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context] | sort' "${config}")"
actual="$(jq -S '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context] | sort' <<<"${live}")"

if [ "${actual}" != "${expected}" ]; then
  echo "live ruleset ${ruleset_id} required checks differ from ${config}" >&2
  echo "expected: ${expected}" >&2
  echo "actual:   ${actual}" >&2
  exit 1
fi

if ! jq -e '.rules[] | select(.type == "required_status_checks") | .parameters.strict_required_status_checks_policy == true' <<<"${live}" >/dev/null; then
  echo "live ruleset ${ruleset_id} does not require checks on the current PR head" >&2
  exit 1
fi
