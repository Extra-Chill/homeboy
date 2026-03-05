#!/usr/bin/env bash

set -euo pipefail

REPO="${GITHUB_REPOSITORY}"
COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"
OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"

if [ -n "${GITHUB_REF_NAME:-}" ]; then
  TRIGGER_CONTEXT="ref \`${GITHUB_REF_NAME}\`"
else
  TRIGGER_CONTEXT="commit \`${GITHUB_SHA:0:8}\`"
fi

FAILED_CMDS=""
IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"
for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)
  STATUS=$(echo "${RESULTS}" | jq -r --arg cmd "${CMD}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown")
  if [ "${STATUS}" = "fail" ]; then
    FAILED_CMDS+="- \`homeboy ${CMD}\`"$'\n'
  fi
done

ISSUE_TITLE="CI failure: homeboy ${COMP_ID} (${GITHUB_EVENT_NAME})"

EXISTING_ISSUE=$(gh api "repos/${REPO}/issues" \
  --jq "[.[] | select(.state == \"open\" and .title == \"${ISSUE_TITLE}\" and (.labels[]?.name == \"ci-failure\"))] | first | .number // empty" \
  2>/dev/null || true)

if [ -n "${EXISTING_ISSUE}" ]; then
  echo "Found existing open issue #${EXISTING_ISSUE} — adding comment instead of creating new issue"

  COMMENT="### CI failure on ${TRIGGER_CONTEXT}"$'\n\n'
  COMMENT+="**Failed commands:**"$'\n'
  COMMENT+="${FAILED_CMDS}"$'\n'
  COMMENT+="**Run:** ${RUN_URL}"$'\n'

  for CMD in "${CMD_ARRAY[@]}"; do
    CMD=$(echo "${CMD}" | xargs)
    STATUS=$(echo "${RESULTS}" | jq -r --arg cmd "${CMD}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown")
    if [ "${STATUS}" = "fail" ] && [ -f "${OUTPUT_DIR}/${CMD}.log" ]; then
      LOG_TAIL=$(tail -30 "${OUTPUT_DIR}/${CMD}.log")
      COMMENT+=$'\n'"<details><summary>${CMD} output (last 30 lines)</summary>"$'\n\n'
      COMMENT+="\`\`\`"$'\n'"${LOG_TAIL}"$'\n'"\`\`\`"$'\n'"</details>"$'\n'
    fi
  done

  gh api "repos/${REPO}/issues/${EXISTING_ISSUE}/comments" \
    --method POST \
    --field body="${COMMENT}" > /dev/null

  echo "Comment added to issue #${EXISTING_ISSUE}"
else
  echo "Creating new issue..."

  BODY="## CI Failure Report"$'\n\n'
  BODY+="**Component:** \`${COMP_ID}\`"$'\n'
  BODY+="**Trigger:** \`${GITHUB_EVENT_NAME}\` on ${TRIGGER_CONTEXT}"$'\n'
  BODY+="**Binary:** ${BINARY_SOURCE}"$'\n'
  BODY+="**Run:** ${RUN_URL}"$'\n\n'
  BODY+="### Failed Commands"$'\n\n'
  BODY+="${FAILED_CMDS}"$'\n'

  for CMD in "${CMD_ARRAY[@]}"; do
    CMD=$(echo "${CMD}" | xargs)
    STATUS=$(echo "${RESULTS}" | jq -r --arg cmd "${CMD}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown")
    if [ "${STATUS}" = "fail" ] && [ -f "${OUTPUT_DIR}/${CMD}.log" ]; then
      LOG_TAIL=$(tail -50 "${OUTPUT_DIR}/${CMD}.log")
      BODY+="### \`homeboy ${CMD}\` output"$'\n\n'
      BODY+="<details><summary>Last 50 lines</summary>"$'\n\n'
      BODY+="\`\`\`"$'\n'"${LOG_TAIL}"$'\n'"\`\`\`"$'\n'"</details>"$'\n\n'
    fi
  done

  BODY+="---"$'\n'
  BODY+="*Filed automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action)*"

  gh api "repos/${REPO}/issues" \
    --method POST \
    --field title="${ISSUE_TITLE}" \
    --field body="${BODY}" \
    --field "labels[]=ci-failure" > /dev/null 2>&1 || {
    gh api "repos/${REPO}/issues" \
      --method POST \
      --field title="${ISSUE_TITLE}" \
      --field body="${BODY}" > /dev/null
  }

  echo "Issue created: ${ISSUE_TITLE}"
fi
