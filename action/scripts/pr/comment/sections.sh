#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/scope/context.sh"
source "${GITHUB_ACTION_PATH}/scripts/pr/comment/lib.sh"

commands_use_review_report() {
  local normalized
  normalized="$(canonicalize_commands "${COMMANDS}")"

  [ "${normalized}" = "audit,lint,test" ] && [ -z "${EXTRA_ARGS:-}" ]
}

add_review_banner() {
  local key="$1"
  local value="$2"

  [ -n "${value}" ] || return 0
  REVIEW_CMD+=(--banner "${key}=${value}")
}

append_review_banner_args() {
  if [ "${AUTOFIX_ENABLED:-false}" = "true" ] && [ "${AUTOFIX_COMMITTED:-}" = "true" ]; then
    local autofix_summary="applied"
    if [ -n "${AUTOFIX_FILE_COUNT:-}" ] && [ -n "${AUTOFIX_FIX_TYPES:-}" ]; then
      autofix_summary+=" — ${AUTOFIX_FILE_COUNT} file(s) fixed via ${AUTOFIX_FIX_TYPES}"
    elif [ -n "${AUTOFIX_FILE_COUNT:-}" ]; then
      autofix_summary+=" — ${AUTOFIX_FILE_COUNT} file(s) fixed"
    fi
    add_review_banner "autofix" "${autofix_summary}"
  elif [ "${AUTOFIX_ENABLED:-false}" = "true" ] && [ "${AUTOFIX_ATTEMPTED:-false}" = "true" ] && [ "${AUTOFIX_STATUS:-}" = "push-failed" ]; then
    add_review_banner "autofix" "generated changes but could not push them back to ${AUTOFIX_TARGET_REPO:-${REPO}}:${AUTOFIX_TARGET_BRANCH:-unknown}"
  elif [ "${AUTOFIX_ENABLED:-false}" = "true" ] && [ "${AUTOFIX_STATUS:-}" = "skipped-head-bot-author" ]; then
    add_review_banner "autofix" "skipped — PR head is already a homeboy-ci[bot] commit, so PR autofix only runs after human commits"
  fi

  if [ "${BINARY_SOURCE:-source}" = "fallback" ]; then
    add_review_banner "binary-source" "fallback release binary (source build failed)"
  fi
}

append_review_report_section() {
  if ! commands_use_review_report; then
    SECTION_BODY+="> :warning: Homeboy core PR-comment rendering currently supports the default \`audit,lint,test\` review report only. Check the action logs for \`${COMMANDS}\`."$'\n\n'
    return 0
  fi

  local review_md review_exit scope_flags
  local -a REVIEW_CMD
  REVIEW_CMD=(homeboy review "${COMP_ID}" --path "${WORKSPACE}" --report=pr-comment)

  scope_flags="$(scope_flags_for "review")"
  if [ -n "${scope_flags}" ]; then
    # shellcheck disable=SC2206
    REVIEW_CMD+=(${scope_flags})
  fi

  append_review_banner_args

  set +e
  review_md="$("${REVIEW_CMD[@]}" 2>/dev/null)"
  review_exit=$?
  set -e

  if [ -z "${review_md}" ]; then
    SECTION_BODY+="> :warning: \`homeboy review --report=pr-comment\` produced no output. Check the action logs for details."$'\n\n'
    return 0
  fi

  if [[ "${review_md}" != *"finding(s) across"* ]]; then
    SECTION_BODY+="> :warning: \`homeboy review --report=pr-comment\` returned an unexpected report shape. Check the action logs for details."$'\n\n'
    return 0
  fi

  SECTION_BODY+="${review_md}"$'\n\n'

  # Exit code 1 means the review found issues, which is exactly when the PR
  # comment is most useful. Exit code >=2 is an execution problem; keep the
  # rendered diagnostics if core emitted any, but surface a workflow warning.
  if [ "${review_exit}" -ge 2 ]; then
    echo "::warning::homeboy review report command exited ${review_exit}; posted rendered diagnostics."
  fi

  return 0
}

build_section_body() {
  SECTION_BODY="### ${SECTION_TITLE}"$'\n\n'
  append_review_report_section
}

# Build the shared `tooling` section body written at the bottom of every
# Homeboy Results comment. This section is re-rendered idempotently by every
# invocation of `post-pr-comment.sh` so versions always reflect the latest
# run. Pinned last via `--section-order lint,build,test,audit,tooling`.
#
# Writes to stdout so the caller can redirect to a tmp file.
build_tooling_section() {
  local cli_version="${HOMEBOY_CLI_VERSION:-unknown}"
  local extension_id="${HOMEBOY_EXTENSION_ID:-auto}"
  local extension_source="${HOMEBOY_EXTENSION_SOURCE:-auto}"
  local extension_revision="${HOMEBOY_EXTENSION_REVISION:-unknown}"
  local action_repository="${HOMEBOY_ACTION_REPOSITORY:-unknown}"
  local action_ref="${HOMEBOY_ACTION_REF:-unknown}"

  printf '<details><summary>Tooling versions</summary>\n\n'
  printf '%s\n' "- Homeboy CLI: \`${cli_version}\`"
  printf '%s\n' "- Extension: \`${extension_id}\` from \`${extension_source}\`"
  printf '%s\n' "- Extension revision: \`${extension_revision}\`"
  printf '%s\n' "- Action: \`${action_repository}@${action_ref}\`"
  printf '\n</details>\n\n'
}
