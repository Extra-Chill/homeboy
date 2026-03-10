#!/usr/bin/env bash

set -euo pipefail

compact_summary() {
  local command="$1"
  local json_file="$2"
  python3 "${GITHUB_ACTION_PATH}/scripts/digest/render-command-summary.py" "${command}" "${json_file}" compact 2>/dev/null || true
}

summary_json_for() {
  case "$1" in
    lint)
      printf '%s\n' "${OUTPUT_DIR}/homeboy-lint-summary.json"
      ;;
    test)
      printf '%s\n' "${OUTPUT_DIR}/homeboy-test-failures.json"
      ;;
    audit)
      printf '%s\n' "${OUTPUT_DIR}/homeboy-audit-summary.json"
      ;;
    *)
      printf '%s\n' ""
      ;;
  esac
}

REPO="${GITHUB_REPOSITORY}"
COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"
OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
WORKFLOW_NAME="${GITHUB_WORKFLOW:-workflow}"
REF_LABEL="${GITHUB_REF_NAME:-${GITHUB_SHA:0:8}}"
AUTOFIX_ATTEMPTED="${AUTOFIX_ATTEMPTED:-false}"
AUTOFIX_PR_CREATED="${AUTOFIX_PR_CREATED:-false}"

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
PRIMARY_SUMMARY=""

for RAW_CMD in "${CMD_ARRAY[@]}"; do
  CMD="$(echo "${RAW_CMD}" | xargs)"
  STATUS=$(echo "${RESULTS}" | jq -r --arg cmd "${CMD}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown")
  if [ "${STATUS}" = "fail" ]; then
    FAILED_COMMANDS+=("${CMD}")
    if [ -z "${PRIMARY_COMMAND}" ]; then
      PRIMARY_COMMAND="${CMD}"
      PRIMARY_JSON="$(summary_json_for "${CMD}")"
      if [ -n "${PRIMARY_JSON}" ] && [ -f "${PRIMARY_JSON}" ]; then
        PRIMARY_SUMMARY="$(compact_summary "${CMD}" "${PRIMARY_JSON}")"
      fi
    fi
  fi
done

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
  if [ -n "${PRIMARY_SUMMARY}" ]; then
    COMMENT+="- Summary: ${PRIMARY_SUMMARY}"$'\n'
  else
    COMMENT+="- Summary: structured failure summary unavailable"$'\n'
  fi

  if [ -n "${SECONDARY_CMDS_MD}" ]; then
    COMMENT+=$'\n'"### Secondary findings"$'\n\n'
    COMMENT+="${SECONDARY_CMDS_MD}"
  fi

  COMMENT+=$'\n'"### Tooling versions"$'\n\n'
  COMMENT+="${TOOLING_MD}"$'\n'

  if [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
    COMMENT+=$'\n'"### Autofix outcome"$'\n\n'
    COMMENT+="- Safe autofix pass was attempted before filing this issue."$'\n'
    COMMENT+="- Remaining failures are likely non-mechanical and need human decision-making."$'\n'
    if [ "${AUTOFIX_PR_CREATED}" = "true" ]; then
      COMMENT+="- Autofix PR was created, but unresolved failures still require follow-up."$'\n'
    fi
  fi

  COMMENT+="### Triage order"$'\n\n'
  COMMENT+="1. Fix \`${PRIMARY_COMMAND:-first failing command}\`"$'\n'
  COMMENT+="2. Re-run CI"$'\n'
  COMMENT+="3. Re-evaluate secondary failures"$'\n\n'

  COMMENT+="**Failed commands:**"$'\n'
  COMMENT+="${FAILED_CMDS_MD}"$'\n'
  COMMENT+="**Run:** ${RUN_URL}"$'\n'

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

  if [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
    BODY+=$'\n'"### Autofix outcome"$'\n\n'
    BODY+="- Safe autofix pass was attempted before filing this issue."$'\n'
    BODY+="- Remaining failures are likely non-mechanical and need human decision-making."$'\n'
    if [ "${AUTOFIX_PR_CREATED}" = "true" ]; then
      BODY+="- Autofix PR was created, but unresolved failures still require follow-up."$'\n'
    fi
  fi

  BODY+="### Primary failure"$'\n\n'
  if [ -n "${PRIMARY_COMMAND}" ]; then
    BODY+="- Command: \`homeboy ${PRIMARY_COMMAND}\`"$'\n'
  fi
  if [ -n "${PRIMARY_SUMMARY}" ]; then
    BODY+="- Summary: ${PRIMARY_SUMMARY}"$'\n'
  else
    BODY+="- Summary: structured failure summary unavailable"$'\n'
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
