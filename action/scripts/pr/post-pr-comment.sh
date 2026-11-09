#!/usr/bin/env bash

# Post a section to the shared Homeboy Results PR comment.
#
# Uses the sectioned PR-comment primitive from Extra-Chill/homeboy#1353:
#   homeboy git pr comment <component> \
#     --comment-key <outer> --section-key <inner> \
#     --body-file <path> --header "..." --footer-file <path> \
#     --section-order <...>
#
# Core handles:
#   - parsing existing sections (new + legacy markers)
#   - merging this invocation's section in place (preserving position)
#   - race consolidation (canonical = lowest id, delete duplicates)
#   - idempotency (identical body → noop, no PATCH)
#   - header/footer preservation and replacement across merges
#
# This script delegates this job's section body to
# `homeboy review --report=pr-comment`, then lets Homeboy core merge the section
# and footer into the shared sticky comment.
#
# Renderer migration tracked in: Extra-Chill/homeboy-action#239.

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"
source "${GITHUB_ACTION_PATH}/scripts/pr/comment/lib.sh"
source "${GITHUB_ACTION_PATH}/scripts/pr/comment/sections.sh"

# Same precedence as select-final-results.sh and generate-failure-digest.sh.
# This consumer was the one left on the raw HOMEBOY_OUTPUT_DIR, which is why
# dropping the duplicate bench copy broke the comment summary rather than any
# of the others.
OUTPUT_DIR="${HOMEBOY_CI_RESULTS_DIR:-${HOMEBOY_OUTPUT_DIR:-}}"
COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
REPO="${GITHUB_REPOSITORY}"

if [ -z "${OUTPUT_DIR}" ] || [ -z "${PR_NUMBER:-}" ]; then
  echo "Skipping PR comment — missing output dir or PR number"
  exit 0
fi

# Reports belong on the run page, including when comments are disabled or the
# caller cannot authenticate. Rendering must not depend on PR write access.
DIGEST_FILE="${HOMEBOY_FAILURE_DIGEST_FILE:-}"
SECTION_TITLE="$(derive_section_title)"
build_section_body
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  { printf '%s\n' "${SECTION_BODY}"; build_tooling_footer; } >> "${GITHUB_STEP_SUMMARY}"
fi
export OUTPUT_DIR
python3 "${GITHUB_ACTION_PATH}/scripts/pr/comment/new-findings.py"
COMMENT_MODE="${PR_COMMENT_INPUT:-on-new-failures}"
case "${COMMENT_MODE}" in
  off|false) echo "Skipping PR comment — comments disabled"; exit 0 ;;
  always|true|on-new-failures) ;;
  *) echo "::warning::Unknown pr-comment mode; refusing to post"; exit 0 ;;
esac

if [ -z "${GH_TOKEN:-}" ]; then
  echo "::warning::Skipping PR comment — app-token was not provided. Refusing to post as github-actions[bot]; configure the homeboy-ci GitHub App token."
  exit 0
fi

if ! pr_is_active; then
  echo "Skipping PR comment — PR #${PR_NUMBER} is no longer open (merged or closed)"
  exit 0
fi

DIGEST_FILE="${HOMEBOY_FAILURE_DIGEST_FILE:-}"
COMMENT_KEY="$(derive_comment_key)"
SECTION_KEY="$(derive_section_key)"
SECTION_TITLE="$(derive_section_title)"
HEADER="## Homeboy Results — \`${COMP_ID}\`"
if is_component_matrix; then
  HEADER="## Homeboy Results"
fi
# Preserve the current rendered section order (lint, build, test, bench).
# Tooling versions are passed as the core-managed footer instead of an action-
# rendered pseudo-section.
SECTION_ORDER="lint,build,test,bench"

if printf '%s' "${RESULTS}" | jq -e 'length > 0 and all(.[]; . == "inconclusive")' >/dev/null; then
  echo "Skipping PR comment — inconclusive differential result"
  exit 0
fi

if [ "${COMMENT_MODE}" != "always" ] && [ "${COMMENT_MODE}" != "true" ]; then
  SECTION_BODY="$(<"${OUTPUT_DIR}/new-failures.md")"
  if [ -z "${SECTION_BODY}" ]; then
    # Never create a green comment. Replace only this invocation's existing
    # section so concurrent matrix components retain their own failures.
    EXISTING="$(gh api --paginate "repos/${REPO}/issues/${PR_NUMBER}/comments" 2>/dev/null || true)"
    if ! printf '%s' "${EXISTING}" | jq -se --arg marker "<!-- homeboy:comment-key=${COMMENT_KEY} -->" \
      'any(.[] | arrays | .[]; (.body // "") | contains($marker))' >/dev/null 2>&1; then
      echo "Skipping PR comment — run passed with no actionable content"
      exit 0
    fi
    # Unknown/inconclusive evidence must never clear a stale failure.
    if ! printf '%s' "${RESULTS}" | jq -e 'all(.[]; . == "pass" or . == "baseline_red")' >/dev/null; then
      echo "Skipping PR comment — no definite new failures"
      exit 0
    fi
    SECTION_BODY="✅ resolved — no new findings"
  fi
fi

SECTION_FILE="$(mktemp)"
TOOLING_FOOTER_FILE="$(mktemp)"
trap 'rm -f "${SECTION_FILE}" "${TOOLING_FOOTER_FILE}"' EXIT

printf '%s' "${SECTION_BODY}" > "${SECTION_FILE}"
# The tooling footer lives exclusively in the step summary.
: > "${TOOLING_FOOTER_FILE}"

# Older pinned binaries do not support the atomic PR lifecycle/head guard.
HEAD_GUARD_ARGS=()
if [ -n "${HEAD_SHA:-}" ]; then
  COMMENT_HELP="$(homeboy git pr comment --help 2>/dev/null || true)"
  if [[ "${COMMENT_HELP}" == *"--expected-head-sha"* ]]; then
    HEAD_GUARD_ARGS=(--expected-head-sha "${HEAD_SHA}")
  fi
fi

# --- Post this job's section -------------------------------------------------
POST_RESULT="$(
  homeboy git pr comment "${COMP_ID}" \
    --path "${WORKSPACE}" \
    --number "${PR_NUMBER}" \
    ${HEAD_GUARD_ARGS[@]+"${HEAD_GUARD_ARGS[@]}"} \
    --comment-key "${COMMENT_KEY}" \
    --section-key "${SECTION_KEY}" \
    --body-file "${SECTION_FILE}" \
    --header "${HEADER}" \
    --footer-file "${TOOLING_FOOTER_FILE}" \
    --section-order "${SECTION_ORDER}" 2>/dev/null || true
)"

if [ -z "${POST_RESULT}" ]; then
  # Most common cause: restricted GITHUB_TOKEN on fork PRs. Warn, continue.
  echo "::warning::Could not post PR comment section '${SECTION_KEY}' (likely restricted token for fork PR)."
  exit 0
fi

POSTED_COMMENT_ID="$(printf '%s' "${POST_RESULT}" | jq -r '.data.comment_id // empty' 2>/dev/null || true)"
POSTED_ACTION="$(printf '%s' "${POST_RESULT}" | jq -r '.data.action // .action // empty' 2>/dev/null || true)"

if [ "${POSTED_ACTION}" = "skipped" ]; then
  SKIP_REASON="$(printf '%s' "${POST_RESULT}" | jq -r '.data.skip_reason // "unknown"' 2>/dev/null || true)"
  if [ "${SKIP_REASON}" = "stale_head" ]; then
    PR_HEAD_SHA="$(printf '%s' "${POST_RESULT}" | jq -r '.data.current_head_sha // "unknown"' 2>/dev/null || true)"
    echo "Skipped PR comment: stale_head (run ${HEAD_SHA:-unknown}, PR head ${PR_HEAD_SHA})"
  else
    echo "Skipped PR comment: ${SKIP_REASON}"
  fi
  exit 0
fi

echo "Posted section '${SECTION_KEY}' to comment #${POSTED_COMMENT_ID:-?} (${POSTED_ACTION:-unknown})"

if [ -n "${POSTED_COMMENT_ID:-}" ]; then
  echo "HOMEBOY_PR_COMMENT_POSTED=true" >> "${GITHUB_ENV}"
  echo "HOMEBOY_PR_COMMENT_ID=${POSTED_COMMENT_ID}" >> "${GITHUB_ENV}"
fi

echo "PR comment posted successfully"
