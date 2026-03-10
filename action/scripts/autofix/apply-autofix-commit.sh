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

# Count autofix commits on this branch only (not full repo history).
# Use scope base ref when available, fall back to origin/main..HEAD,
# then full log as last resort.
# Match the prefix (not full subject) so informative suffixes don't break detection.
BASE="$(scope_base_ref)"
if [ -n "${BASE}" ]; then
  AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep "^${AUTOFIX_COMMIT_PREFIX}" "${BASE}..HEAD" 2>/dev/null | wc -l | xargs)
elif [ -n "${GITHUB_BASE_REF:-}" ]; then
  AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep "^${AUTOFIX_COMMIT_PREFIX}" "origin/${GITHUB_BASE_REF}..HEAD" 2>/dev/null | wc -l | xargs)
else
  AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep "^${AUTOFIX_COMMIT_PREFIX}" | wc -l | xargs)
fi
if [ "${AUTOFIX_COMMIT_COUNT}" -ge "${AUTOFIX_MAX_COMMITS}" ]; then
  echo "Skipping autofix: reached max autofix commits (${AUTOFIX_COMMIT_COUNT}/${AUTOFIX_MAX_COMMITS})"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [ "${GITHUB_ACTOR:-}" = "github-actions[bot]" ] || [ "${GITHUB_ACTOR:-}" = "homeboy-ci[bot]" ]; then
  echo "Skipping autofix: workflow actor is ${GITHUB_ACTOR} (bot loop guard)"
  echo "attempted=false" >> "${GITHUB_OUTPUT}"
  echo "status=skipped-bot-loop" >> "${GITHUB_OUTPUT}"
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
  echo "Applying autofixes..."
  for FIX_CMD in "${FIX_ARRAY[@]}"; do
    FIX_CMD=$(echo "${FIX_CMD}" | xargs)
    BASE_CMD="$(build_autofix_command "${FIX_CMD}" "${COMP_ID}" "${WORKSPACE}")"

    echo "Running autofix: ${BASE_CMD}"
    set +e
    eval "${BASE_CMD}"
    FIX_EXIT=$?
    set -e

    if [ "${FIX_EXIT}" -ne 0 ]; then
      echo "Autofix command exited non-zero (${FIX_EXIT}), continuing to check for file changes"
    fi
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

  if [ -n "${HOMEBOY_OUTPUT_DIR:-}" ] && [ -d "${HOMEBOY_OUTPUT_DIR}" ]; then
    AUTOFIX_FINDING_TYPES="$(jq -r '
      [
        .. | .fix_summary? // empty | .rules? // empty | .[]? | .rule? // empty
      ]
      | map(select(type == "string" and length > 0))
      | unique
      | sort
      | join(", ")
    ' "${HOMEBOY_OUTPUT_DIR}"/*.json 2>/dev/null | tail -n 1)"
  fi

  COMMIT_MSG="$(build_autofix_commit_message "${AUTOFIX_FIX_TYPES}" "${AUTOFIX_FILE_COUNT}" "${AUTOFIX_FINDING_TYPES}")"

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
  # Derive refactor sources from the command list, but enforce canonical order:
  # audit → lint → test. In Homeboy, fix = refactor, so the action should call
  # canonical refactor source passes instead of command-specific fix modes.
  HAS_AUDIT=false HAS_LINT=false HAS_TEST=false
  IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"
  for CMD in "${CMD_ARRAY[@]}"; do
    CMD=$(echo "${CMD}" | xargs)
    BASE_CMD=$(printf '%s' "${CMD}" | awk '{print $1}')
    case "${BASE_CMD}" in
      audit) HAS_AUDIT=true ;;
      lint)  HAS_LINT=true ;;
      test)  HAS_TEST=true ;;
    esac
  done
  FIX_ARRAY=()
  [ "${HAS_AUDIT}" = true ] && FIX_ARRAY+=("refactor --from audit --write")
  [ "${HAS_LINT}" = true ]  && FIX_ARRAY+=("refactor --from lint --write")
  [ "${HAS_TEST}" = true ]  && FIX_ARRAY+=("refactor --from test --write")
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

  run_autofixes
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
