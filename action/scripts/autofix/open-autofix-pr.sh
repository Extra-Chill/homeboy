#!/usr/bin/env bash

# Find-or-create/edit an autofix PR using `homeboy git pr` primitives.
#
# Primitives: Extra-Chill/homeboy#1334 (issue/PR CRUD), #1368 (--path flag).
# Migration tracked in: Extra-Chill/homeboy-action#138.

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

if [ -z "${AUTOFIX_BRANCH:-}" ]; then
  echo "No autofix branch provided; skipping PR creation"
  echo "created=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [[ "${GITHUB_REF:-}" == refs/heads/* ]]; then
  BASE_BRANCH="${GITHUB_REF#refs/heads/}"
else
  BASE_BRANCH="$(gh api "repos/${GITHUB_REPOSITORY}" --jq '.default_branch' 2>/dev/null || echo 'main')"
fi
COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"

TITLE="chore(ci): autofix ${COMP_ID} from ${BASE_BRANCH}"
BODY_FILE="$(mktemp)"
trap 'rm -f "${BODY_FILE}"' EXIT

AUTOFIX_DETAIL=""
if [ -n "${AUTOFIX_FILE_COUNT:-}" ] && [ -n "${AUTOFIX_FIX_TYPES:-}" ]; then
  AUTOFIX_DETAIL="- **${AUTOFIX_FILE_COUNT}** file(s) fixed via **${AUTOFIX_FIX_TYPES}**"
elif [ -n "${AUTOFIX_FILE_COUNT:-}" ]; then
  AUTOFIX_DETAIL="- **${AUTOFIX_FILE_COUNT}** file(s) fixed"
fi

FINDING_DETAIL=""
if [ -n "${AUTOFIX_FINDING_TYPES:-}" ]; then
  FINDING_DETAIL="- **Finding categories:** ${AUTOFIX_FINDING_TYPES}"
fi

cat > "${BODY_FILE}" <<EOF
## Summary
${AUTOFIX_DETAIL:+${AUTOFIX_DETAIL}
}${FINDING_DETAIL:+${FINDING_DETAIL}
}- Opened immediately after autofix without rerunning quality gates.
- Generated automatically by Homeboy Action.

## Context
- Workflow run: ${RUN_URL}
- Branch: ${AUTOFIX_BRANCH}
- Base: ${BASE_BRANCH}
EOF

# Find an existing open PR for (base, head). `homeboy git pr find` emits typed
# JSON with a stable shape regardless of `gh` version; jq extracts the first
# item's number + url.
EXISTING_PR=$(homeboy git pr find "${COMP_ID}" \
  --path "${WORKSPACE}" \
  --base "${BASE_BRANCH}" \
  --head "${AUTOFIX_BRANCH}" \
  --state open \
  --limit 1 2>/dev/null \
  | jq -c '.data.items[0] // empty' 2>/dev/null || true)

if [ -n "${EXISTING_PR}" ]; then
  EXISTING_PR_NUMBER=$(echo "${EXISTING_PR}" | jq -r '.number // empty')
  EXISTING_PR_URL=$(echo "${EXISTING_PR}" | jq -r '.url // empty')

  echo "Autofix PR already exists: ${EXISTING_PR_URL}"

  if [ -n "${EXISTING_PR_NUMBER}" ]; then
    if homeboy git pr edit "${COMP_ID}" \
      --path "${WORKSPACE}" \
      --number "${EXISTING_PR_NUMBER}" \
      --body-file "${BODY_FILE}" >/dev/null 2>&1; then
      echo "Updated PR #${EXISTING_PR_NUMBER} body with latest run context"
    else
      echo "::warning::Failed to update PR #${EXISTING_PR_NUMBER} body"
    fi
  fi

  echo "created=true" >> "${GITHUB_OUTPUT}"
  echo "url=${EXISTING_PR_URL}" >> "${GITHUB_OUTPUT}"
  exit 0
fi

# No existing PR — create one.
PR_URL=$(homeboy git pr create "${COMP_ID}" \
  --path "${WORKSPACE}" \
  --base "${BASE_BRANCH}" \
  --head "${AUTOFIX_BRANCH}" \
  --title "${TITLE}" \
  --body-file "${BODY_FILE}" 2>/dev/null \
  | jq -r '.data.url // empty' 2>/dev/null || true)

if [ -z "${PR_URL}" ]; then
  echo "Failed to create autofix PR"
  echo "created=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

echo "Autofix PR created: ${PR_URL}"
echo "created=true" >> "${GITHUB_OUTPUT}"
echo "url=${PR_URL}" >> "${GITHUB_OUTPUT}"
