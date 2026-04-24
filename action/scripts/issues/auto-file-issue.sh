#!/usr/bin/env bash

# File (or comment on) an issue describing a CI failure.
#
# Uses `homeboy git issue` primitives:
#   - `issue find --title ... --label ci-failure --state open` for dedup
#   - `issue comment --number N` to add a follow-up to an existing issue
#   - `issue create --title ... --body-file ... --label ci-failure` otherwise
#
# Primitives: Extra-Chill/homeboy#1334 (issue/PR CRUD), #1368 (--path flag).
# Migration tracked in: Extra-Chill/homeboy-action#138.

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

compact_summary() {
  local command="$1"
  local json_file="$2"
  python3 "${GITHUB_ACTION_PATH}/scripts/digest/render-command-summary.py" "${command}" "${json_file}" compact 2>/dev/null || true
}

summary_json_for() {
  # Resolve the structured --output JSON written by run-homeboy-commands.sh.
  # All commands use the same output stem convention.
  local stem
  stem="$(printf '%s' "$1" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  if [ -n "${OUTPUT_DIR}" ] && [ -f "${OUTPUT_DIR}/${stem}.json" ]; then
    printf '%s\n' "${OUTPUT_DIR}/${stem}.json"
  else
    printf '%s\n' ""
  fi
}

REPO="${GITHUB_REPOSITORY}"
COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
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

# Dedup: is there already an open ci-failure issue with this exact title?
# `issue find --title --label --state` does the exact-match + all-of-labels
# filtering that the previous `gh api ... --jq` line reinvented.
EXISTING_ISSUE=$(homeboy git issue find "${COMP_ID}" \
  --path "${WORKSPACE}" \
  --title "${ISSUE_TITLE}" \
  --label ci-failure \
  --state open \
  --limit 1 2>/dev/null \
  | jq -r '.data.items[0].number // empty' 2>/dev/null || true)

if [ -n "${EXISTING_ISSUE}" ]; then
  echo "Found existing open issue #${EXISTING_ISSUE} — adding comment instead of creating new issue"

  COMMENT_FILE="$(mktemp)"
  trap 'rm -f "${COMMENT_FILE}"' EXIT

  {
    printf '### CI failure on %s\n\n' "${TRIGGER_CONTEXT}"
    printf '### Primary failure\n\n'
    if [ -n "${PRIMARY_COMMAND}" ]; then
      printf -- '- Command: `homeboy %s`\n' "${PRIMARY_COMMAND}"
    fi
    if [ -n "${PRIMARY_SUMMARY}" ]; then
      printf -- '- Summary: %s\n' "${PRIMARY_SUMMARY}"
    else
      printf -- '- Summary: structured failure summary unavailable\n'
    fi

    if [ -n "${SECONDARY_CMDS_MD}" ]; then
      printf '\n### Secondary findings\n\n%s' "${SECONDARY_CMDS_MD}"
    fi

    printf '\n### Tooling versions\n\n%s\n' "${TOOLING_MD}"

    if [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
      printf '\n### Autofix outcome\n\n'
      printf -- '- Safe autofix pass was attempted before filing this issue.\n'
      printf -- '- Remaining failures are likely non-mechanical and need human decision-making.\n'
      if [ "${AUTOFIX_PR_CREATED}" = "true" ]; then
        printf -- '- Autofix PR was created, but unresolved failures still require follow-up.\n'
      fi
    fi

    printf '### Triage order\n\n'
    printf '1. Fix `%s`\n' "${PRIMARY_COMMAND:-first failing command}"
    printf '2. Re-run CI\n'
    printf '3. Re-evaluate secondary failures\n\n'

    printf '**Failed commands:**\n%s\n' "${FAILED_CMDS_MD}"
    printf '**Run:** %s\n' "${RUN_URL}"
  } > "${COMMENT_FILE}"

  homeboy git issue comment "${COMP_ID}" \
    --path "${WORKSPACE}" \
    --number "${EXISTING_ISSUE}" \
    --body-file "${COMMENT_FILE}" >/dev/null

  echo "Comment added to issue #${EXISTING_ISSUE}"
else
  echo "Creating new issue..."

  BODY_FILE="$(mktemp)"
  trap 'rm -f "${BODY_FILE}"' EXIT

  {
    printf '## CI Failure Report\n\n'
    printf '**Component:** `%s`\n' "${COMP_ID}"
    printf '**Workflow:** `%s`\n' "${WORKFLOW_NAME}"
    printf '**Scope:** `%s`\n' "${SCOPE_LABEL}"
    printf '**Ref:** `%s`\n' "${REF_LABEL}"
    printf '**Trigger:** `%s` on %s\n' "${GITHUB_EVENT_NAME}" "${TRIGGER_CONTEXT}"
    printf '**Binary:** %s\n' "${BINARY_SOURCE}"
    printf '**Run:** %s\n\n' "${RUN_URL}"

    printf '### Tooling versions\n\n%s\n' "${TOOLING_MD}"

    if [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
      printf '\n### Autofix outcome\n\n'
      printf -- '- Safe autofix pass was attempted before filing this issue.\n'
      printf -- '- Remaining failures are likely non-mechanical and need human decision-making.\n'
      if [ "${AUTOFIX_PR_CREATED}" = "true" ]; then
        printf -- '- Autofix PR was created, but unresolved failures still require follow-up.\n'
      fi
    fi

    printf '### Primary failure\n\n'
    if [ -n "${PRIMARY_COMMAND}" ]; then
      printf -- '- Command: `homeboy %s`\n' "${PRIMARY_COMMAND}"
    fi
    if [ -n "${PRIMARY_SUMMARY}" ]; then
      printf -- '- Summary: %s\n' "${PRIMARY_SUMMARY}"
    else
      printf -- '- Summary: structured failure summary unavailable\n'
    fi

    if [ -n "${SECONDARY_CMDS_MD}" ]; then
      printf '\n### Secondary findings\n\n%s' "${SECONDARY_CMDS_MD}"
    fi

    printf '\n### Triage order\n\n'
    printf '1. Fix `%s`\n' "${PRIMARY_COMMAND:-first failing command}"
    printf '2. Re-run CI\n'
    printf '3. Re-evaluate secondary failures\n\n'

    printf '### Failed Commands\n\n%s\n' "${FAILED_CMDS_MD}"

    printf '%s\n' '---'
    printf '%s' '*Filed automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action)*'
  } > "${BODY_FILE}"

  homeboy git issue create "${COMP_ID}" \
    --path "${WORKSPACE}" \
    --title "${ISSUE_TITLE}" \
    --body-file "${BODY_FILE}" \
    --label ci-failure >/dev/null

  echo "Issue created: ${ISSUE_TITLE}"
fi
