#!/usr/bin/env bash

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
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"

EXISTING_PR_URL=$(gh pr list \
  --state open \
  --base "${BASE_BRANCH}" \
  --head "${AUTOFIX_BRANCH}" \
  --json url \
  --jq '.[0].url // empty' 2>/dev/null || true)

if [ -n "${EXISTING_PR_URL}" ]; then
  echo "Autofix PR already exists: ${EXISTING_PR_URL}"

  # Update the PR body with the latest run context (branch was force-pushed
  # with fresh autofix changes, so the old workflow run link is stale).
  EXISTING_PR_NUMBER=$(gh pr list \
    --state open \
    --base "${BASE_BRANCH}" \
    --head "${AUTOFIX_BRANCH}" \
    --json number \
    --jq '.[0].number // empty' 2>/dev/null || true)

  if [ -n "${EXISTING_PR_NUMBER}" ]; then
    UPDATE_BODY_FILE="$(mktemp)"

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

cat > "${UPDATE_BODY_FILE}" <<UPDATEBODY
## Summary
${AUTOFIX_DETAIL:+${AUTOFIX_DETAIL}
}${FINDING_DETAIL:+${FINDING_DETAIL}
}- Opened immediately after autofix without rerunning quality gates.
- Generated automatically by Homeboy Action.

## Context
- Workflow run: ${RUN_URL}
- Branch: ${AUTOFIX_BRANCH}
- Base: ${BASE_BRANCH}
UPDATEBODY

    gh pr edit "${EXISTING_PR_NUMBER}" --body-file "${UPDATE_BODY_FILE}" 2>/dev/null || true
    rm -f "${UPDATE_BODY_FILE}"
    echo "Updated PR #${EXISTING_PR_NUMBER} body with latest run context"
  fi

  echo "created=true" >> "${GITHUB_OUTPUT}"
  echo "url=${EXISTING_PR_URL}" >> "${GITHUB_OUTPUT}"
  exit 0
fi

TITLE="chore(ci): autofix ${COMP_ID} from ${BASE_BRANCH}"
BODY_FILE="$(mktemp)"

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

PR_URL=$(gh pr create \
  --base "${BASE_BRANCH}" \
  --head "${AUTOFIX_BRANCH}" \
  --title "${TITLE}" \
  --body-file "${BODY_FILE}" 2>/dev/null || true)

rm -f "${BODY_FILE}"

if [ -z "${PR_URL}" ]; then
  echo "Failed to create autofix PR"
  echo "created=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

echo "Autofix PR created: ${PR_URL}"
echo "created=true" >> "${GITHUB_OUTPUT}"
echo "url=${PR_URL}" >> "${GITHUB_OUTPUT}"
