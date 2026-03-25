#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/scope/context.sh"
source "${GITHUB_ACTION_PATH}/scripts/pr/comment/lib.sh"

append_autofix_section() {
  if ! is_refactor_owning_section; then
    return 0
  fi

  if [ "${AUTOFIX_ENABLED}" = "true" ] && [ "${AUTOFIX_COMMITTED:-}" = "true" ]; then
    local autofix_summary=":wrench: **Autofix applied**"
    if [ -n "${AUTOFIX_FILE_COUNT:-}" ] && [ -n "${AUTOFIX_FIX_TYPES:-}" ]; then
      autofix_summary+=" — ${AUTOFIX_FILE_COUNT} file(s) fixed via ${AUTOFIX_FIX_TYPES}"
    elif [ -n "${AUTOFIX_FILE_COUNT:-}" ]; then
      autofix_summary+=" — ${AUTOFIX_FILE_COUNT} file(s) fixed"
    fi
    SECTION_BODY+="> ${autofix_summary}"$'\n\n'
  elif [ "${AUTOFIX_ENABLED}" = "true" ] && [ "${AUTOFIX_ATTEMPTED:-false}" = "true" ] && [ "${AUTOFIX_STATUS:-}" = "push-failed" ]; then
    SECTION_BODY+="> :warning: Autofix generated changes but could not push them back to **${AUTOFIX_TARGET_REPO:-${REPO}}:${AUTOFIX_TARGET_BRANCH:-unknown}**"$'\n\n'
  elif [ "${AUTOFIX_ENABLED}" = "true" ] && [ "${AUTOFIX_STATUS:-}" = "skipped-head-bot-author" ]; then
    SECTION_BODY+="> :information_source: Autofix skipped — PR head is already a **homeboy-ci[bot]** commit, so PR autofix only runs after human commits"$'\n\n'
  elif [ "${AUTOFIX_ENABLED}" = "true" ]; then
    SECTION_BODY+="> :information_source: Autofix enabled, but no fixable file changes were produced"$'\n\n'
  fi
}

append_binary_source_section() {
  if [ "${BINARY_SOURCE}" = "fallback" ]; then
    SECTION_BODY+="> :warning: **Source build failed** — results from fallback release binary"$'\n\n'
  fi
}

append_digest_section() {
  HAS_DIGEST="false"
  if [ -n "${DIGEST_FILE}" ] && [ -f "${DIGEST_FILE}" ]; then
    HAS_DIGEST="true"
    SECTION_BODY+="$(cat "${DIGEST_FILE}")"$'\n\n'
  fi
}

append_scope_section() {
  if is_scoped; then
    SECTION_BODY+="> :zap: Scope: **changed files only**"$'\n\n'
  elif [ "$(scope_context)" = "pr" ] && [ "${SCOPE_MODE:-full}" = "full" ]; then
    SECTION_BODY+="> :information_source: Scope resolved to **full**"$'\n\n'
  fi
}

append_test_scope_section() {
  if [[ ",${COMMANDS}," == *",test,"* ]] || [[ "${COMMANDS}" == "test" ]]; then
    if [ "$(scope_context)" = "pr" ] && [ "${SCOPE_MODE:-full}" = "full" ]; then
      SECTION_BODY+="> :information_source: PR test scope: **full**"$'\n\n'
    elif [ "${TEST_SCOPE_EFFECTIVE:-}" = "changed" ]; then
      SECTION_BODY+="> :zap: PR test scope: **changed** (files affected by this PR)"$'\n\n'
    fi
  fi
}

append_command_sections() {
  IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"

  for CMD in "${CMD_ARRAY[@]}"; do
    CMD=$(echo "${CMD}" | xargs)
    SUMMARY_JSON="$(summary_json_for_command "${CMD}")"
    STATUS="$(command_status "${CMD}")"
    ICON="$(command_icon "${STATUS}")"
    SCOPE_NOTE="$(scope_note_for "${CMD}")"

    SECTION_BODY+=":${ICON}: **${CMD}**${SCOPE_NOTE}"$'\n'

    if digest_covers_command "${CMD}"; then
      :
    elif [ -n "${SUMMARY_JSON}" ] && [ -f "${SUMMARY_JSON}" ]; then
      SUMMARY_MD="$(render_structured_summary "${CMD}" "${SUMMARY_JSON}")"
      if [ -n "${SUMMARY_MD}" ]; then
        SECTION_BODY+="${SUMMARY_MD}"$'\n'
      fi
    elif [ "${STATUS}" = "fail" ] && [ "${HAS_DIGEST}" != "true" ]; then
      SECTION_BODY+="- No structured ${CMD} summary artifact was generated."$'\n'
    fi

    SECTION_BODY+=$'\n'
  done
}

build_section_body() {
  SECTION_BODY="### ${SECTION_TITLE}"$'\n\n'
  append_autofix_section
  append_binary_source_section
  append_digest_section
  append_scope_section
  append_test_scope_section
  append_command_sections
}
