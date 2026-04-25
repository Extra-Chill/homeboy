#!/usr/bin/env bash

# Post a section to the shared Homeboy Results PR comment.
#
# Uses the sectioned PR-comment primitive from Extra-Chill/homeboy#1353:
#   homeboy git pr comment <component> \
#     --comment-key <outer> --section-key <inner> \
#     --body-file <path> --header "..." --section-order <...>
#
# Core handles:
#   - parsing existing sections (new + legacy markers)
#   - merging this invocation's section in place (preserving position)
#   - race consolidation (canonical = lowest id, delete duplicates)
#   - idempotency (identical body → noop, no PATCH)
#   - header preservation across merges
#
# This script only handles presentation (body rendering via sections.sh) and
# two primitive calls: one for this job's section, one for the shared
# "tooling" section (re-rendered every run so versions stay fresh).
#
# Migration tracked in: Extra-Chill/homeboy-action#141.

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"
source "${GITHUB_ACTION_PATH}/scripts/pr/comment/lib.sh"
source "${GITHUB_ACTION_PATH}/scripts/pr/comment/sections.sh"

OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
REPO="${GITHUB_REPOSITORY}"

if [ -z "${OUTPUT_DIR}" ] || [ -z "${PR_NUMBER:-}" ]; then
  echo "Skipping PR comment — missing output dir or PR number"
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
# Preserve the current rendered order (lint, build, test, audit). Core's
# default is alphabetical; `tooling` is pinned last so the versions block
# stays at the bottom of the comment.
SECTION_ORDER="lint,build,test,audit,tooling"

build_section_body

SECTION_FILE="$(mktemp)"
TOOLING_FILE="$(mktemp)"
trap 'rm -f "${SECTION_FILE}" "${TOOLING_FILE}"' EXIT

printf '%s' "${SECTION_BODY}" > "${SECTION_FILE}"
build_tooling_section > "${TOOLING_FILE}"

# --- Post this job's section -------------------------------------------------
POST_RESULT="$(
  homeboy git pr comment "${COMP_ID}" \
    --path "${WORKSPACE}" \
    --number "${PR_NUMBER}" \
    --comment-key "${COMMENT_KEY}" \
    --section-key "${SECTION_KEY}" \
    --body-file "${SECTION_FILE}" \
    --header "${HEADER}" \
    --section-order "${SECTION_ORDER}" 2>/dev/null || true
)"

if [ -z "${POST_RESULT}" ]; then
  # Most common cause: restricted GITHUB_TOKEN on fork PRs. Warn, continue.
  echo "::warning::Could not post PR comment section '${SECTION_KEY}' (likely restricted token for fork PR)."
  exit 0
fi

POSTED_COMMENT_ID="$(printf '%s' "${POST_RESULT}" | jq -r '.data.comment_id // empty' 2>/dev/null || true)"
POSTED_ACTION="$(printf '%s' "${POST_RESULT}" | jq -r '.action // empty' 2>/dev/null || true)"

echo "Posted section '${SECTION_KEY}' to comment #${POSTED_COMMENT_ID:-?} (${POSTED_ACTION:-unknown})"

if [ -n "${POSTED_COMMENT_ID:-}" ]; then
  echo "HOMEBOY_PR_COMMENT_POSTED=true" >> "${GITHUB_ENV}"
  echo "HOMEBOY_PR_COMMENT_ID=${POSTED_COMMENT_ID}" >> "${GITHUB_ENV}"
fi

# --- Refresh the shared `tooling` section -----------------------------------
# Every invocation idempotently rewrites the tooling footer with current env
# values. Core's noop-on-identical-body guard means this is free when nothing
# changed. Race-safe: last writer wins, content is always derivable from env.
TOOLING_RESULT="$(
  homeboy git pr comment "${COMP_ID}" \
    --path "${WORKSPACE}" \
    --number "${PR_NUMBER}" \
    --comment-key "${COMMENT_KEY}" \
    --section-key "tooling" \
    --body-file "${TOOLING_FILE}" \
    --header "${HEADER}" \
    --section-order "${SECTION_ORDER}" 2>/dev/null || true
)"

if [ -z "${TOOLING_RESULT}" ]; then
  echo "::warning::Could not refresh tooling section (continuing; section may be stale)."
else
  TOOLING_ACTION="$(printf '%s' "${TOOLING_RESULT}" | jq -r '.action // empty' 2>/dev/null || true)"
  echo "Refreshed tooling section (${TOOLING_ACTION:-unknown})"
fi

echo "PR comment posted successfully"
