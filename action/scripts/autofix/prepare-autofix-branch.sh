#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

AUTOFIX_MAX_COMMITS="${AUTOFIX_MAX_COMMITS:-2}"
if ! [[ "${AUTOFIX_MAX_COMMITS}" =~ ^[0-9]+$ ]]; then
  AUTOFIX_MAX_COMMITS=2
fi

# Count consecutive autofix commits at HEAD. A human commit resets the counter.
# This prevents runaway autofix-PR loops while allowing autofix to resume after
# any human merge. The old approach (count all autofix commits since last tag)
# permanently tripped the guard after N total historical autofix commits across
# all PRs, blocking all future autofix even after human intervention.
# When the cap is hit, code fixes are skipped but baseline updates still run —
# baseline updates use a distinct commit prefix and don't count toward the cap.
AUTOFIX_COMMIT_COUNT=0
while IFS= read -r subject; do
  if [[ "${subject}" == "${AUTOFIX_COMMIT_PREFIX}"* ]]; then
    AUTOFIX_COMMIT_COUNT=$((AUTOFIX_COMMIT_COUNT + 1))
  else
    break
  fi
done < <(git log --format=%s -n "$((AUTOFIX_MAX_COMMITS + 1))" 2>/dev/null)
AUTOFIX_CAP_HIT=false
if [ "${AUTOFIX_COMMIT_COUNT}" -ge "${AUTOFIX_MAX_COMMITS}" ]; then
  echo "Autofix cap reached: ${AUTOFIX_COMMIT_COUNT} consecutive autofix commits at HEAD (max ${AUTOFIX_MAX_COMMITS})"
  echo "Skipping code fixes but will still attempt baseline update"
  AUTOFIX_CAP_HIT=true
fi

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

# Check if a previous autofix commit was reverted in recent history.
# A revert signals broken autofix output — back off until a human intervenes.
if has_reverted_autofix; then
  echo "Skipping non-PR autofix: a previous autofix commit was reverted in recent history"
  echo "This indicates the autofix output was incorrect — manual review required."
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

# Run code fixes only when under the cap
if [ "${AUTOFIX_CAP_HIT}" = false ]; then
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
else
  echo "Skipping code fixes (autofix cap reached)"
fi

# Update baseline so it stays current when this commit merges to main.
# Full (unscoped) audit ensures the baseline reflects the entire codebase,
# not just changed files. Tolerate failure — baseline update is best-effort.
# This runs even when the autofix cap is hit — baseline updates use a distinct
# commit prefix and don't count toward the cap.
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
AUTOFIX_CHANGED_FILES="$(git diff --cached --name-only | sort)"

# Detect baseline-only changes: only homeboy.json modified while cap was hit
BASELINE_ONLY=false
if [ "${AUTOFIX_CAP_HIT}" = true ]; then
  if echo "${AUTOFIX_CHANGED_FILES}" | grep -qx "homeboy.json" && [ "${AUTOFIX_FILE_COUNT}" -eq 1 ]; then
    BASELINE_ONLY=true
  fi
fi

if [ "${BASELINE_ONLY}" = true ]; then
  # Use a distinct commit prefix so it doesn't count toward the autofix cap
  COMMIT_MSG="chore(ci): update audit baseline

Baseline-only update (autofix cap reached, code fixes skipped).
homeboy.json"
else
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
fi
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
