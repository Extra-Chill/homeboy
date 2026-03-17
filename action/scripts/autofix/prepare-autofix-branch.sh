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

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"

if [ -n "${AUTOFIX_COMMANDS:-}" ]; then
  IFS=',' read -ra FIX_ARRAY <<< "${AUTOFIX_COMMANDS}"
else
  # Derive refactor sources from the command list, but enforce canonical order:
  # audit → lint → test. In Homeboy, fix = refactor, so non-PR autofix should
  # also use canonical refactor source passes.
  HAS_AUDIT=false HAS_LINT=false HAS_TEST=false
  IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"
  for CMD in "${CMD_ARRAY[@]}"; do
    CMD=$(echo "${CMD}" | xargs)
    case "${CMD}" in
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
  echo "No non-PR autofix commands configured for this command set"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

AUTOFIX_BRANCH="ci/autofix/${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"

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

# Validate that autofix changes compile before committing.
# The validate_write gate catches this locally, but CI autofix runs through
# a different path. This prevents shipping broken decompositions (#832).
if [ "${BASELINE_ONLY}" != true ]; then
  if ! validate_autofix_compilation "${WORKSPACE}" "${COMP_ID}"; then
    echo "Aborting autofix commit — rolling back staged changes"
    git reset HEAD -- .
    git checkout -- .
    git clean -fd 2>/dev/null || true
    git checkout -
    git branch -D "${AUTOFIX_BRANCH}"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    exit 0
  fi
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

  # Extract finding categories from autofix JSON output.
  # Looks for fix_summary.rules[].rule or data.findings[].kind in the output files.
  AUTOFIX_FINDING_TYPES=""
  if [ -d "${AUTOFIX_OUTPUT_DIR}" ]; then
    AUTOFIX_FINDING_TYPES="$(jq -r '
      [
        .. | .fix_summary? // empty | .rules? // empty | .[]? | .rule? // empty
      ]
      | map(select(type == "string" and length > 0))
      | unique
      | sort
      | join(", ")
    ' "${AUTOFIX_OUTPUT_DIR}"/*.json 2>/dev/null | tail -n 1)"
  fi

  COMMIT_MSG="$(build_autofix_commit_message "${AUTOFIX_FIX_TYPES}" "${AUTOFIX_FILE_COUNT}" "${AUTOFIX_FINDING_TYPES}")"
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
if [ -n "${APP_TOKEN:-}" ]; then
  echo "Pushing with GitHub App token (will trigger CI re-run)"
  git -c "http.https://github.com/.extraheader=" \
    push "https://x-access-token:${APP_TOKEN}@github.com/${GITHUB_REPOSITORY}.git" \
    "${AUTOFIX_BRANCH}"
else
  echo "Pushing with default token (will NOT trigger CI re-run)"
  git push origin "${AUTOFIX_BRANCH}"
fi

echo "committed=true" >> "${GITHUB_OUTPUT}"
echo "autofix-branch=${AUTOFIX_BRANCH}" >> "${GITHUB_OUTPUT}"
echo "autofix-file-count=${AUTOFIX_FILE_COUNT}" >> "${GITHUB_OUTPUT}"
echo "autofix-fix-types=${AUTOFIX_FIX_TYPES:-}" >> "${GITHUB_OUTPUT}"
echo "autofix-finding-types=${AUTOFIX_FINDING_TYPES:-}" >> "${GITHUB_OUTPUT}"
