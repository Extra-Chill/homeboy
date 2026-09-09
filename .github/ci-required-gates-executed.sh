#!/usr/bin/env bash
set -euo pipefail

head_sha="${REQUIRED_GATES_HEAD_SHA:-${GITHUB_SHA:-unknown}}"
results="${CI_GATE_RESULTS:-}"

if ! jq -e 'type == "object"' >/dev/null 2>&1 <<<"${results}"; then
  echo "::error::required-gates-executed requires CI_GATE_RESULTS=toJSON(needs)"
  exit 1
fi

failed="$(jq -r 'to_entries | map(select(.value.result != "success") | "\(.key)=\(.value.result // "missing")") | join(", ")' <<<"${results}")"
missing=''
for job in rustfmt lint homeboy; do
  if ! jq -e --arg job "${job}" 'has($job)' >/dev/null <<<"${results}"; then
    missing="${missing}${missing:+, }${job}"
  fi
done
echo "required-gates-executed head=${head_sha} needs=$(jq -cS . <<<"${results}")"

if [ -n "${failed}" ] || [ -n "${missing}" ]; then
  echo "::error::required-gates-executed: required jobs did not all succeed for head ${head_sha}: ${failed} missing=[${missing}]"
  exit 1
fi

echo "::notice::required-gates-executed: all required jobs succeeded for head ${head_sha}"
