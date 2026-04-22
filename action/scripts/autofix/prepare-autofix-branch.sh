#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

# ── CI-only pre-flight guards ──
# Domain guards (revert detection, bot HEAD detection, cap enforcement)
# live in homeboy core's guard.rs and run during `homeboy refactor --write`.

# Hard guards — these always exit (no baseline bypass). Bot actors and autofix
# branches risk infinite loops, so bail entirely.
if [ "${GITHUB_ACTOR:-}" = "github-actions[bot]" ] || [ "${GITHUB_ACTOR:-}" = "homeboy-ci[bot]" ]; then
  echo "Skipping non-PR autofix: workflow actor is ${GITHUB_ACTOR} (bot loop guard)"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [[ "${GITHUB_REF_NAME:-}" == ci/autofix/* ]]; then
  echo "Skipping non-PR autofix: already on an autofix branch (${GITHUB_REF_NAME})"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"

if [ -n "${AUTOFIX_COMMANDS:-}" ]; then
  IFS=',' read -ra FIX_ARRAY <<< "${AUTOFIX_COMMANDS}"
else
  # The refactor command is the single source of truth for all code fixes.
  # --from all runs audit → lint → test sources sequentially in one invocation,
  # so later stages see earlier stages' modifications.
  FIX_ARRAY=("refactor --from all --write")
fi

if [ ${#FIX_ARRAY[@]} -eq 0 ]; then
  echo "No non-PR autofix commands configured for this command set"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

# Stable branch name: reuse across cron runs so open-autofix-pr.sh dedup works.
# Old behavior used GITHUB_RUN_ID which created a new branch (and PR) every run.
if [[ "${GITHUB_REF:-}" == refs/heads/* ]]; then
  BASE_BRANCH="${GITHUB_REF#refs/heads/}"
else
  BASE_BRANCH="$(gh api "repos/${GITHUB_REPOSITORY}" --jq '.default_branch' 2>/dev/null || echo 'main')"
fi
AUTOFIX_BRANCH="ci/autofix/${COMP_ID}/${BASE_BRANCH}"

# If the branch already exists remotely (from a previous cron run), delete it
# and start fresh from HEAD. The old branch may be stale or conflict with
# current main. Force-pushing a fresh branch is simpler and safer than rebasing.
if git ls-remote --exit-code --heads origin "${AUTOFIX_BRANCH}" >/dev/null 2>&1; then
  echo "Remote branch ${AUTOFIX_BRANCH} already exists; will force-push fresh changes"
fi

echo "Creating autofix branch: ${AUTOFIX_BRANCH}"
git checkout -b "${AUTOFIX_BRANCH}"

AUTOFIX_OUTPUT_DIR=$(mktemp -d)

echo "Applying non-PR autofixes..."
FIX_INDEX=0
for FIX_CMD in "${FIX_ARRAY[@]}"; do
  FIX_CMD=$(echo "${FIX_CMD}" | xargs)
  OUTPUT_FILE="${AUTOFIX_OUTPUT_DIR}/fix-${FIX_INDEX}.json"
  BASE_CMD="$(build_autofix_command "${FIX_CMD}" "${COMP_ID}" "${WORKSPACE}" "${OUTPUT_FILE}")"

  echo "Running autofix: ${BASE_CMD}"
  set +e
  eval "${BASE_CMD}"
  FIX_EXIT=$?
  set -e

  if [ "${FIX_EXIT}" -ne 0 ]; then
    echo "Autofix command exited non-zero (${FIX_EXIT}), continuing to inspect generated changes"
  fi
  FIX_INDEX=$((FIX_INDEX + 1))
done

# Check if homeboy core's guards blocked the refactor.
# This reads the JSON output from the autofix run.
json_files=$(find "${AUTOFIX_OUTPUT_DIR}" -name '*.json' -type f 2>/dev/null)
if [ -n "${json_files}" ]; then
  guard_status="$(jq -r '[.[] | .data // . | select(type == "object") | .guard_block // empty | .reason] | first // empty' ${json_files} 2>/dev/null || true)"
  if [ -n "${guard_status}" ]; then
    echo "Skipping non-PR autofix: core guard blocked — ${guard_status}"
    rm -rf "${AUTOFIX_OUTPUT_DIR}"
    git checkout - 2>/dev/null || true
    git branch -D "${AUTOFIX_BRANCH}" 2>/dev/null || true
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    exit 0
  fi
fi

# Update baseline so it stays current when this commit merges to main.
# Full (unscoped) audit ensures the baseline reflects the entire codebase,
# not just changed files. Tolerate failure — baseline update is best-effort.
echo "Updating audit baseline..."
set +e
homeboy audit "${COMP_ID}" --baseline --path "${WORKSPACE}"
BASELINE_EXIT=$?
set -e
if [ "${BASELINE_EXIT}" -ne 0 ]; then
  echo "Baseline update exited non-zero (${BASELINE_EXIT}), continuing"
fi

if git diff --quiet && git diff --cached --quiet; then
  echo "No non-PR autofix changes detected"
  git checkout -
  git branch -D "${AUTOFIX_BRANCH}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

git add -A
if git diff --cached --quiet; then
  echo "No staged changes after non-PR autofix"
  git checkout -
  git branch -D "${AUTOFIX_BRANCH}"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

# Capture autofix summary: file count, fix commands, and finding categories
AUTOFIX_FILE_COUNT=$(git diff --cached --name-only | wc -l | xargs)

AUTOFIX_FIX_TYPES=""
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
if [ -d "${AUTOFIX_OUTPUT_DIR}" ]; then
  raw_details="$(extract_fix_details_from_output "${AUTOFIX_OUTPUT_DIR}")"
  if [ -n "${raw_details}" ]; then
    AUTOFIX_TOTAL_FIXES="$(echo "${raw_details}" | head -1)"
    AUTOFIX_DETAILS="$(echo "${raw_details}" | tail -n +2)"
  fi
fi

COMMIT_MSG="$(build_autofix_commit_message "${AUTOFIX_FIX_TYPES}" "${AUTOFIX_FILE_COUNT}" "${AUTOFIX_DETAILS}" "${AUTOFIX_TOTAL_FIXES}")"
rm -rf "${AUTOFIX_OUTPUT_DIR}"

BOT_NAME="homeboy-ci[bot]"
BOT_EMAIL="266378653+homeboy-ci[bot]@users.noreply.github.com"
git config user.name "${BOT_NAME}"
git config user.email "${BOT_EMAIL}"
GIT_AUTHOR_NAME="${BOT_NAME}" \
GIT_AUTHOR_EMAIL="${BOT_EMAIL}" \
GIT_COMMITTER_NAME="${BOT_NAME}" \
GIT_COMMITTER_EMAIL="${BOT_EMAIL}" \
  git commit -m "${COMMIT_MSG}"

# Use GitHub App token for push if available — pushes from a GitHub App
# trigger workflow re-runs, while GITHUB_TOKEN pushes do not.
# Force-push because the stable branch name may already exist from a previous
# cron run. The fresh commit replaces the stale one.
if [ -n "${APP_TOKEN:-}" ]; then
  echo "Pushing with GitHub App token (will trigger CI re-run)"
  git -c "http.https://github.com/.extraheader=" \
    push --force "https://x-access-token:${APP_TOKEN}@github.com/${GITHUB_REPOSITORY}.git" \
    "${AUTOFIX_BRANCH}"
else
  echo "Pushing with default token (will NOT trigger CI re-run)"
  git push --force origin "${AUTOFIX_BRANCH}"
fi

echo "committed=true" >> "${GITHUB_OUTPUT}"
echo "autofix-branch=${AUTOFIX_BRANCH}" >> "${GITHUB_OUTPUT}"
echo "autofix-file-count=${AUTOFIX_FILE_COUNT}" >> "${GITHUB_OUTPUT}"
echo "autofix-fix-types=${AUTOFIX_FIX_TYPES:-}" >> "${GITHUB_OUTPUT}"
echo "autofix-finding-types=${AUTOFIX_FINDING_TYPES:-}" >> "${GITHUB_OUTPUT}"
