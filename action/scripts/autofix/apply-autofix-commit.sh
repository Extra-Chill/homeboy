#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"
source "${GITHUB_ACTION_PATH}/scripts/scope/context.sh"

AUTOFIX_MAX_COMMITS="${AUTOFIX_MAX_COMMITS:-2}"
if ! [[ "${AUTOFIX_MAX_COMMITS}" =~ ^[0-9]+$ ]]; then
  AUTOFIX_MAX_COMMITS=2
fi

AUTOFIX_PUSH_ATTEMPTS="${AUTOFIX_PUSH_ATTEMPTS:-3}"
if ! [[ "${AUTOFIX_PUSH_ATTEMPTS}" =~ ^[0-9]+$ ]]; then
  AUTOFIX_PUSH_ATTEMPTS=3
fi

# Count autofix commits on THIS PR branch only.
# Scope: base_ref..HEAD counts only commits added by this PR, so autofix
# commits on other PRs or merged to main never count against this branch.
# Fallback: count consecutive autofix commits at HEAD (same logic as non-PR)
# so we never accidentally count the entire repo history.
BASE="$(scope_base_ref)"
if [ -n "${BASE}" ]; then
  AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep "^${AUTOFIX_COMMIT_PREFIX}" "${BASE}..HEAD" 2>/dev/null | wc -l | xargs)
elif [ -n "${GITHUB_BASE_REF:-}" ]; then
  AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep "^${AUTOFIX_COMMIT_PREFIX}" "origin/${GITHUB_BASE_REF}..HEAD" 2>/dev/null | wc -l | xargs)
else
  # No base ref available — count consecutive autofix commits at HEAD.
  # This prevents false positives from historical autofix commits on other branches.
  AUTOFIX_COMMIT_COUNT=0
  while IFS= read -r subject; do
    if [[ "${subject}" == "${AUTOFIX_COMMIT_PREFIX}"* ]]; then
      AUTOFIX_COMMIT_COUNT=$((AUTOFIX_COMMIT_COUNT + 1))
    else
      break
    fi
  done < <(git log --format=%s -n "$((AUTOFIX_MAX_COMMITS + 1))" 2>/dev/null)
fi
# When the cap is hit, code fixes are skipped but baseline updates still run
# (non-PR only — PR context skips baseline in maybe_update_baseline).
AUTOFIX_CAP_HIT=false
if [ "${AUTOFIX_COMMIT_COUNT}" -ge "${AUTOFIX_MAX_COMMITS}" ]; then
  echo "Autofix cap reached: ${AUTOFIX_COMMIT_COUNT} autofix commits on this branch (max ${AUTOFIX_MAX_COMMITS})"
  if [ "$(scope_context)" = "pr" ]; then
    echo "Skipping autofix entirely (PR context — baseline updates also skip on PRs)"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    exit 0
  fi
  echo "Skipping code fixes but will still attempt baseline update (non-PR context)"
  AUTOFIX_CAP_HIT=true
fi

if [ "${GITHUB_ACTOR:-}" = "github-actions[bot]" ] || [ "${GITHUB_ACTOR:-}" = "homeboy-ci[bot]" ]; then
  echo "Skipping autofix: workflow actor is ${GITHUB_ACTOR} (bot loop guard)"
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-bot-loop" >> "${GITHUB_OUTPUT}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if ! pr_is_active; then
  echo "Skipping autofix: PR #${PR_NUMBER} is no longer open (merged or closed)"
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-pr-closed" >> "${GITHUB_OUTPUT}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

LAST_SUBJECT=$(git log -1 --pretty=%s 2>/dev/null || true)
if [[ "${LAST_SUBJECT}" == "${AUTOFIX_COMMIT_PREFIX}"* ]]; then
  echo "Skipping autofix: HEAD already an autofix commit"
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-head-autofix" >> "${GITHUB_OUTPUT}"
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

fetch_latest_target_head() {
  local fetch_url
  fetch_url="$(build_github_remote_url "${TARGET_REPO}" "${APP_TOKEN:-}")"
  git fetch --no-tags --depth=1 "${fetch_url}" "+refs/heads/${TARGET_BRANCH}:${TARGET_REF}"
}

reset_to_target_head() {
  git reset --hard "${TARGET_REF}"
  git clean -fd
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

  # Detect baseline-only changes: only homeboy.json modified while cap was hit
  local baseline_only=false
  if [ "${AUTOFIX_CAP_HIT}" = true ]; then
    if echo "${AUTOFIX_CHANGED_FILES}" | grep -qx "homeboy.json" && [ "${AUTOFIX_FILE_COUNT}" -eq 1 ]; then
      baseline_only=true
    fi
  fi

  if [ "${baseline_only}" = true ]; then
    # Use a distinct commit prefix so it doesn't count toward the autofix cap
    COMMIT_MSG="chore(ci): update audit baseline

Baseline-only update (autofix cap reached, code fixes skipped).
homeboy.json"
  else
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
  fi

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
fi

PUSH_ATTEMPT=1
while [ "${PUSH_ATTEMPT}" -le "${AUTOFIX_PUSH_ATTEMPTS}" ]; do
  if git show-ref --verify --quiet "${TARGET_REF}"; then
    reset_to_target_head
  fi

  # Run code fixes only when under the cap
  if [ "${AUTOFIX_CAP_HIT}" = false ]; then
    run_autofixes
  else
    echo "Skipping code fixes (autofix cap reached)"
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

  if [ "${PUSH_ATTEMPT}" -ge "${AUTOFIX_PUSH_ATTEMPTS}" ]; then
    break
  fi

  echo "Autofix push failed on attempt ${PUSH_ATTEMPT}/${AUTOFIX_PUSH_ATTEMPTS}; refetching latest PR head and recomputing"

  # Re-check PR state before retrying — it may have been merged while we were fixing
  if ! pr_is_active; then
    echo "PR #${PR_NUMBER} was merged or closed during autofix — aborting retries"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    echo "status=skipped-pr-closed" >> "${GITHUB_OUTPUT}"
    exit 0
  fi

  fetch_latest_target_head || true
  PUSH_ATTEMPT=$((PUSH_ATTEMPT + 1))
done

echo "::warning::Autofix changes were generated but could not be pushed to ${TARGET_REPO}:${TARGET_BRANCH}"
echo "committed=false" >> "${GITHUB_OUTPUT}"
echo "status=push-failed" >> "${GITHUB_OUTPUT}"
echo "target-repo=${TARGET_REPO}" >> "${GITHUB_OUTPUT}"
echo "target-branch=${TARGET_BRANCH}" >> "${GITHUB_OUTPUT}"
echo "autofix-file-count=${AUTOFIX_FILE_COUNT:-0}" >> "${GITHUB_OUTPUT}"
echo "autofix-fix-types=${AUTOFIX_FIX_TYPES:-}" >> "${GITHUB_OUTPUT}"
