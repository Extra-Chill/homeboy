#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"
source "${GITHUB_ACTION_PATH}/scripts/scope/context.sh"

# ── CI-only pre-flight guards ──
# These guards are CI-specific (actor identity, stale binary, PR state)
# and have no equivalent in homeboy core. Domain guards (revert detection,
# bot HEAD detection, cap enforcement, disabled-label) live in core's
# guard.rs and are checked during `homeboy refactor --write`.

if [ "${GITHUB_ACTOR:-}" = "github-actions[bot]" ] || [ "${GITHUB_ACTOR:-}" = "homeboy-ci[bot]" ]; then
  echo "Skipping autofix: workflow actor is ${GITHUB_ACTOR} (bot loop guard)"
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-bot-loop" >> "${GITHUB_OUTPUT}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [ -n "${AUTOFIX_LABEL:-}" ]; then
  if ! echo "${PR_LABELS_JSON}" | jq -e --arg label "${AUTOFIX_LABEL}" 'index($label) != null' > /dev/null; then
    echo "Skipping autofix: required label '${AUTOFIX_LABEL}' not present"
    echo "attempted=false" >> "${GITHUB_OUTPUT}"
    echo "status=skipped-missing-label" >> "${GITHUB_OUTPUT}"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    exit 0
  fi
fi

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
TARGET_REPO="$(resolve_pr_target_repo)"
TARGET_BRANCH="$(resolve_pr_target_branch)"
TARGET_REF="refs/remotes/homeboy-autofix-target/${TARGET_BRANCH}"
ORIGINAL_HEAD_SHA="$(git rev-parse HEAD 2>/dev/null || true)"
BUILT_HEAD_SHA="${HOMEBOY_CLI_HEAD_SHA:-}"

if [ -n "${BUILT_HEAD_SHA}" ] && [ -n "${ORIGINAL_HEAD_SHA}" ] && [ "${BUILT_HEAD_SHA}" != "${ORIGINAL_HEAD_SHA}" ]; then
  echo "Skipping autofix: installed homeboy binary was built from ${BUILT_HEAD_SHA}, but checkout is at ${ORIGINAL_HEAD_SHA}" 
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-stale-binary" >> "${GITHUB_OUTPUT}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

fetch_latest_target_head() {
  local fetch_url
  fetch_url="$(build_github_remote_url "${TARGET_REPO}" "${APP_TOKEN:-}")"
  git fetch --no-tags --depth=1 "${fetch_url}" "+refs/heads/${TARGET_BRANCH}:${TARGET_REF}"
}

reset_to_target_head() {
  git reset --hard "${TARGET_REF}"
  git clean -fd
}

# CI-only post-sync guards: PR state and bot-subject detection.
# Domain guards (revert, cap, disabled-label, force-push) run inside
# homeboy refactor --write via guard.rs and surface as guard_block in JSON.
guard_synced_pr_head() {
  if ! pr_is_active; then
    echo "Skipping autofix: PR #${PR_NUMBER} is no longer open (merged or closed)"
    echo "attempted=false" >> "${GITHUB_OUTPUT}"
    echo "status=skipped-pr-closed" >> "${GITHUB_OUTPUT}"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    exit 0
  fi
}

# Check the refactor JSON output for a guard_block from homeboy core.
# Returns 0 if blocked (and sets guard status outputs), 1 if clear.
check_core_guard_block() {
  local output_dir="$1"

  local json_files
  json_files=$(find "${output_dir}" -name '*.json' -type f 2>/dev/null)
  [ -n "${json_files}" ] || return 1

  local guard_status
  guard_status="$(jq -r '[.[] | .data // . | select(type == "object") | .guard_block // empty | .reason] | first // empty' ${json_files} 2>/dev/null || true)"

  if [ -n "${guard_status}" ]; then
    # Map core's kebab-case reason to the action's skipped-* status convention
    local status="skipped-guard"
    case "${guard_status}" in
      reverted)        status="skipped-reverted" ;;
      force-pushed)    status="skipped-force-pushed" ;;
      disabled-label)  status="skipped-disabled-label" ;;
      head-is-bot-commit) status="skipped-head-bot-author" ;;
      cap-reached)     status="skipped-cap-reached" ;;
    esac

    local guard_message
    guard_message="$(jq -r '[.[] | .data // . | select(type == "object") | .guard_block // empty | .message // .reason] | first // "unknown"' ${json_files} 2>/dev/null || echo "unknown")"

    echo "Skipping autofix: core guard blocked — ${guard_message}"
    echo "attempted=true" >> "${GITHUB_OUTPUT}"
    echo "status=${status}" >> "${GITHUB_OUTPUT}"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    return 0
  fi

  return 1
}

run_autofixes() {
  AUTOFIX_OUTPUT_DIR=$(mktemp -d)
  echo "Applying autofixes..."
  local fix_index=0
  for FIX_CMD in "${FIX_ARRAY[@]}"; do
    FIX_CMD=$(echo "${FIX_CMD}" | xargs)
    local output_file="${AUTOFIX_OUTPUT_DIR}/fix-${fix_index}.json"
    BASE_CMD="$(build_autofix_command "${FIX_CMD}" "${COMP_ID}" "${WORKSPACE}" "${output_file}")"

    echo "Running autofix: ${BASE_CMD}"
    set +e
    eval "${BASE_CMD}"
    FIX_EXIT=$?
    set -e

    if [ "${FIX_EXIT}" -ne 0 ]; then
      echo "Autofix command exited non-zero (${FIX_EXIT}), continuing to check for file changes"
    fi
    fix_index=$((fix_index + 1))
  done
}

maybe_update_baseline() {
  if [ "$(scope_context)" != "pr" ]; then
    echo "Updating audit baseline (non-PR context)..."
    set +e
    BASELINE_CMD="homeboy audit ${COMP_ID} --baseline --path ${WORKSPACE}"
    eval "${BASELINE_CMD}"
    BASELINE_EXIT=$?
    set -e
    if [ "${BASELINE_EXIT}" -ne 0 ]; then
      echo "Baseline update exited non-zero (${BASELINE_EXIT}), continuing"
    fi
  else
    echo "Skipping baseline update on PR branch (avoids homeboy.json merge conflicts)"
  fi
}

stage_autofix_changes() {
  if git diff --quiet && git diff --cached --quiet; then
    return 1
  fi

  git add -A
  if git diff --cached --quiet; then
    return 1
  fi

  return 0
}

commit_autofix_changes() {
  AUTOFIX_FILE_COUNT=$(git diff --cached --name-only | wc -l | xargs)
  AUTOFIX_CHANGED_FILES="$(git diff --cached --name-only | sort)"
  AUTOFIX_FIX_TYPES=""
  AUTOFIX_FINDING_TYPES=""

  for FIX_CMD in "${FIX_ARRAY[@]}"; do
    FIX_CMD=$(echo "${FIX_CMD}" | xargs)
    BASE=$(echo "${FIX_CMD}" | awk '{print $1}')
    if [ -n "${AUTOFIX_FIX_TYPES}" ]; then
      AUTOFIX_FIX_TYPES+=", "
    fi
    AUTOFIX_FIX_TYPES+="${BASE}"
  done

  AUTOFIX_DETAILS=""
  AUTOFIX_TOTAL_FIXES=""
  # Read fix details from the autofix run's own output (AUTOFIX_OUTPUT_DIR),
  # not the diagnostic run's output (HOMEBOY_OUTPUT_DIR). The autofix run
  # captures --output with the actual fix results and fixer categories.
  local details_dir="${AUTOFIX_OUTPUT_DIR:-${HOMEBOY_OUTPUT_DIR:-}}"
  if [ -n "${details_dir}" ] && [ -d "${details_dir}" ]; then
    local raw_details
    raw_details="$(extract_fix_details_from_output "${details_dir}")"
    if [ -n "${raw_details}" ]; then
      AUTOFIX_TOTAL_FIXES="$(echo "${raw_details}" | head -1)"
      AUTOFIX_DETAILS="$(echo "${raw_details}" | tail -n +2)"
    fi
  fi

  COMMIT_MSG="$(build_autofix_commit_message "${AUTOFIX_FIX_TYPES}" "${AUTOFIX_FILE_COUNT}" "${AUTOFIX_DETAILS}" "${AUTOFIX_TOTAL_FIXES}")"

  BOT_NAME="homeboy-ci[bot]"
  BOT_EMAIL="266378653+homeboy-ci[bot]@users.noreply.github.com"
  git config user.name "${BOT_NAME}"
  git config user.email "${BOT_EMAIL}"
  GIT_AUTHOR_NAME="${BOT_NAME}" \
  GIT_AUTHOR_EMAIL="${BOT_EMAIL}" \
  GIT_COMMITTER_NAME="${BOT_NAME}" \
  GIT_COMMITTER_EMAIL="${BOT_EMAIL}" \
    git commit -m "${COMMIT_MSG}"
}

push_autofix_commit() {
  local push_target
  push_target="$(resolve_push_target "${TARGET_REPO}" "${APP_TOKEN:-}")"

  if [ -n "${APP_TOKEN:-}" ]; then
    echo "Pushing autofix to ${TARGET_REPO}:${TARGET_BRANCH} with GitHub App token"
  else
    echo "Pushing autofix to ${TARGET_REPO}:${TARGET_BRANCH}"
  fi

  git -c "http.https://github.com/.extraheader=" push "${push_target}" "HEAD:refs/heads/${TARGET_BRANCH}"
}

if [ -n "${AUTOFIX_COMMANDS:-}" ]; then
  IFS=',' read -ra FIX_ARRAY <<< "${AUTOFIX_COMMANDS}"
else
  # The refactor command is the single source of truth for all code fixes.
  # --from all runs audit → lint → test sources sequentially in one invocation,
  # so later stages see earlier stages' modifications.
  FIX_ARRAY=("refactor --from all --write")
fi

if [ ${#FIX_ARRAY[@]} -eq 0 ]; then
  echo "No autofix commands configured for this command set"
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-no-commands" >> "${GITHUB_OUTPUT}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

echo "attempted=true" >> "${GITHUB_OUTPUT}"

if ! fetch_latest_target_head; then
  echo "::warning::Could not fetch latest PR head ${TARGET_REPO}:${TARGET_BRANCH}; using current checkout"
else
  echo "Fetched latest PR head ${TARGET_REPO}:${TARGET_BRANCH}"
  if git show-ref --verify --quiet "${TARGET_REF}"; then
    TARGET_HEAD_SHA="$(git rev-parse "${TARGET_REF}" 2>/dev/null || true)"
    if [ -n "${BUILT_HEAD_SHA}" ] && [ -n "${TARGET_HEAD_SHA}" ] && [ "${BUILT_HEAD_SHA}" != "${TARGET_HEAD_SHA}" ]; then
      echo "Skipping autofix: installed homeboy binary was built from ${BUILT_HEAD_SHA}, but latest PR head is ${TARGET_HEAD_SHA}"
      echo "attempted=false" >> "${GITHUB_OUTPUT}"
      echo "status=skipped-stale-binary" >> "${GITHUB_OUTPUT}"
      echo "committed=false" >> "${GITHUB_OUTPUT}"
      exit 0
    fi
  fi
fi

guard_synced_pr_head

if git show-ref --verify --quiet "${TARGET_REF}"; then
  reset_to_target_head
fi

guard_synced_pr_head

run_autofixes

# Check if homeboy core's guards blocked the refactor.
# This reads the JSON output from the autofix run. Core's guard.rs runs
# inside refactor --write and surfaces guard_block when blocked.
if [ -n "${AUTOFIX_OUTPUT_DIR:-}" ] && check_core_guard_block "${AUTOFIX_OUTPUT_DIR}"; then
  exit 0
fi

maybe_update_baseline

if ! stage_autofix_changes; then
  echo "No autofix changes detected"
  echo "status=no-changes" >> "${GITHUB_OUTPUT}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

commit_autofix_changes

if push_autofix_commit; then
  echo "committed=true" >> "${GITHUB_OUTPUT}"
  echo "status=pushed" >> "${GITHUB_OUTPUT}"
  echo "target-repo=${TARGET_REPO}" >> "${GITHUB_OUTPUT}"
  echo "target-branch=${TARGET_BRANCH}" >> "${GITHUB_OUTPUT}"
  echo "autofix-file-count=${AUTOFIX_FILE_COUNT}" >> "${GITHUB_OUTPUT}"
  echo "autofix-fix-types=${AUTOFIX_FIX_TYPES}" >> "${GITHUB_OUTPUT}"
  exit 0
fi

echo "::warning::Autofix changes were generated but could not be pushed to ${TARGET_REPO}:${TARGET_BRANCH}"
echo "committed=false" >> "${GITHUB_OUTPUT}"
echo "status=push-failed" >> "${GITHUB_OUTPUT}"
echo "target-repo=${TARGET_REPO}" >> "${GITHUB_OUTPUT}"
echo "target-branch=${TARGET_BRANCH}" >> "${GITHUB_OUTPUT}"
echo "autofix-file-count=${AUTOFIX_FILE_COUNT:-0}" >> "${GITHUB_OUTPUT}"
echo "autofix-fix-types=${AUTOFIX_FIX_TYPES:-}" >> "${GITHUB_OUTPUT}"
