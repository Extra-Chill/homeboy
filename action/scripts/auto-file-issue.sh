#!/usr/bin/env bash

set -euo pipefail

REPO="${GITHUB_REPOSITORY}"
COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"
OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
WORKFLOW_NAME="${GITHUB_WORKFLOW:-workflow}"
REF_LABEL="${GITHUB_REF_NAME:-${GITHUB_SHA:0:8}}"

HOMEBOY_CLI_VERSION="${HOMEBOY_CLI_VERSION:-unknown}"
HOMEBOY_EXTENSION_ID="${HOMEBOY_EXTENSION_ID:-auto}"
HOMEBOY_EXTENSION_SOURCE="${HOMEBOY_EXTENSION_SOURCE:-auto}"
HOMEBOY_EXTENSION_REVISION="${HOMEBOY_EXTENSION_REVISION:-unknown}"
HOMEBOY_ACTION_REF="${HOMEBOY_ACTION_REF:-unknown}"
HOMEBOY_ACTION_REPOSITORY="${HOMEBOY_ACTION_REPOSITORY:-unknown}"

REPO_NAME="${REPO##*/}"
if [ "${COMP_ID}" = "${REPO_NAME}" ]; then
  SCOPE_LABEL="${REPO}"
else
  SCOPE_LABEL="${REPO}/${COMP_ID}"
fi

if [ -n "${GITHUB_REF_NAME:-}" ]; then
  TRIGGER_CONTEXT="ref \`${GITHUB_REF_NAME}\`"
else
  TRIGGER_CONTEXT="commit \`${GITHUB_SHA:0:8}\`"
fi

IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"

FAILED_COMMANDS=()
PRIMARY_COMMAND=""
PRIMARY_LOG_FILE=""
PRIMARY_FATAL_LINE=""

for RAW_CMD in "${CMD_ARRAY[@]}"; do
  CMD="$(echo "${RAW_CMD}" | xargs)"
  STATUS=$(echo "${RESULTS}" | jq -r --arg cmd "${CMD}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown")
  if [ "${STATUS}" = "fail" ]; then
    FAILED_COMMANDS+=("${CMD}")
    if [ -z "${PRIMARY_COMMAND}" ]; then
      PRIMARY_COMMAND="${CMD}"
      PRIMARY_LOG_FILE="${OUTPUT_DIR}/${CMD}.log"
    fi
  fi
done

if [ -n "${PRIMARY_LOG_FILE}" ] && [ -f "${PRIMARY_LOG_FILE}" ]; then
  PRIMARY_FATAL_LINE=$(grep -m1 -E "(PHP Fatal error:|Fatal error:|Unhandled exception|panic:|BUILD FAILED:)" "${PRIMARY_LOG_FILE}" || true)
fi

FAILED_CMDS_MD=""
for CMD in "${FAILED_COMMANDS[@]}"; do
  FAILED_CMDS_MD+="- \`homeboy ${CMD}\`"$'\n'
done

SECONDARY_CMDS_MD=""
if [ "${#FAILED_COMMANDS[@]}" -gt 1 ]; then
  for CMD in "${FAILED_COMMANDS[@]}"; do
    if [ "${CMD}" != "${PRIMARY_COMMAND}" ]; then
      SECONDARY_CMDS_MD+="- \`homeboy ${CMD}\`: failed (treat as secondary until primary is resolved)"$'\n'
    fi
  done
fi

TOOLING_MD=""
TOOLING_MD+="- Homeboy CLI: \`${HOMEBOY_CLI_VERSION}\`"$'\n'
TOOLING_MD+="- Extension: \`${HOMEBOY_EXTENSION_ID}\` from \`${HOMEBOY_EXTENSION_SOURCE}\`"$'\n'
TOOLING_MD+="- Extension revision: \`${HOMEBOY_EXTENSION_REVISION}\`"$'\n'
TOOLING_MD+="- Action: \`${HOMEBOY_ACTION_REPOSITORY}@${HOMEBOY_ACTION_REF}\`"$'\n'

ISSUE_TITLE="CI failure: ${SCOPE_LABEL} • ${WORKFLOW_NAME} • ${GITHUB_EVENT_NAME} • ${REF_LABEL}"

EXISTING_ISSUE=$(gh api "repos/${REPO}/issues" \
  --jq "[.[] | select(.state == \"open\" and .title == \"${ISSUE_TITLE}\" and (.labels[]?.name == \"ci-failure\"))] | first | .number // empty" \
  2>/dev/null || true)

if [ -n "${EXISTING_ISSUE}" ]; then
  echo "Found existing open issue #${EXISTING_ISSUE} — adding comment instead of creating new issue"

  COMMENT="### CI failure on ${TRIGGER_CONTEXT}"$'\n\n'
  COMMENT+="### Primary failure"$'\n\n'
  if [ -n "${PRIMARY_COMMAND}" ]; then
    COMMENT+="- Command: \`homeboy ${PRIMARY_COMMAND}\`"$'\n'
  fi
  if [ -n "${PRIMARY_FATAL_LINE}" ]; then
    COMMENT+="- First fatal/error: \`${PRIMARY_FATAL_LINE}\`"$'\n'
  else
    COMMENT+="- First fatal/error: not detected in log tail"$'\n'
  fi

  if [ -n "${SECONDARY_CMDS_MD}" ]; then
    COMMENT+=$'\n'"### Secondary findings"$'\n\n'
    COMMENT+="${SECONDARY_CMDS_MD}"
  fi

  COMMENT+=$'\n'"### Tooling versions"$'\n\n'
  COMMENT+="${TOOLING_MD}"$'\n'

  COMMENT+="### Triage order"$'\n\n'
  COMMENT+="1. Fix \`${PRIMARY_COMMAND:-first failing command}\`"$'\n'
  COMMENT+="2. Re-run CI"$'\n'
  COMMENT+="3. Re-evaluate secondary failures"$'\n\n'

  COMMENT+="**Failed commands:**"$'\n'
  COMMENT+="${FAILED_CMDS_MD}"$'\n'
  COMMENT+="**Run:** ${RUN_URL}"$'\n'

  for RAW_CMD in "${CMD_ARRAY[@]}"; do
    CMD="$(echo "${RAW_CMD}" | xargs)"
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
  BODY+="**Workflow:** \`${WORKFLOW_NAME}\`"$'\n'
  BODY+="**Scope:** \`${SCOPE_LABEL}\`"$'\n'
  BODY+="**Ref:** \`${REF_LABEL}\`"$'\n'
  BODY+="**Trigger:** \`${GITHUB_EVENT_NAME}\` on ${TRIGGER_CONTEXT}"$'\n'
  BODY+="**Binary:** ${BINARY_SOURCE}"$'\n'
  BODY+="**Run:** ${RUN_URL}"$'\n\n'

  BODY+="### Tooling versions"$'\n\n'
  BODY+="${TOOLING_MD}"$'\n'

  BODY+="### Primary failure"$'\n\n'
  if [ -n "${PRIMARY_COMMAND}" ]; then
    BODY+="- Command: \`homeboy ${PRIMARY_COMMAND}\`"$'\n'
  fi
  if [ -n "${PRIMARY_FATAL_LINE}" ]; then
    BODY+="- First fatal/error: \`${PRIMARY_FATAL_LINE}\`"$'\n'
  else
    BODY+="- First fatal/error: not detected in log tail"$'\n'
  fi

  if [ -n "${SECONDARY_CMDS_MD}" ]; then
    BODY+=$'\n'"### Secondary findings"$'\n\n'
    BODY+="${SECONDARY_CMDS_MD}"
  fi

  BODY+=$'\n'"### Triage order"$'\n\n'
  BODY+="1. Fix \`${PRIMARY_COMMAND:-first failing command}\`"$'\n'
  BODY+="2. Re-run CI"$'\n'
  BODY+="3. Re-evaluate secondary failures"$'\n\n'

  BODY+="### Failed Commands"$'\n\n'
  BODY+="${FAILED_CMDS_MD}"$'\n'

  for RAW_CMD in "${CMD_ARRAY[@]}"; do
    CMD="$(echo "${RAW_CMD}" | xargs)"
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
