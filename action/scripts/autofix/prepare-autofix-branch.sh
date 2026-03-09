#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

AUTOFIX_MAX_COMMITS="${AUTOFIX_MAX_COMMITS:-2}"
if ! [[ "${AUTOFIX_MAX_COMMITS}" =~ ^[0-9]+$ ]]; then
  AUTOFIX_MAX_COMMITS=2
fi

AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep "^${AUTOFIX_COMMIT_PREFIX}" | wc -l | xargs)
if [ "${AUTOFIX_COMMIT_COUNT}" -ge "${AUTOFIX_MAX_COMMITS}" ]; then
  echo "Skipping non-PR autofix: reached max autofix commits (${AUTOFIX_COMMIT_COUNT}/${AUTOFIX_MAX_COMMITS})"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

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
  # Derive fix commands from the command list, but enforce canonical order:
  # audit → lint → test. Audit produces structural changes (missing files,
  # baselines), lint fixes style on the resulting code, test stubs come last.
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
  [ "${HAS_AUDIT}" = true ] && FIX_ARRAY+=("audit --fix --write")
  [ "${HAS_LINT}" = true ]  && FIX_ARRAY+=("lint --fix")
  [ "${HAS_TEST}" = true ]  && FIX_ARRAY+=("test --fix")
fi

if [ ${#FIX_ARRAY[@]} -eq 0 ]; then
  echo "No non-PR autofix commands configured for this command set"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

AUTOFIX_BRANCH="ci/autofix/${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"

echo "Creating autofix branch: ${AUTOFIX_BRANCH}"
git checkout -b "${AUTOFIX_BRANCH}"

echo "Applying non-PR autofixes..."
for FIX_CMD in "${FIX_ARRAY[@]}"; do
  FIX_CMD=$(echo "${FIX_CMD}" | xargs)
  BASE_CMD="$(build_autofix_command "${FIX_CMD}" "${COMP_ID}" "${WORKSPACE}")"

  echo "Running autofix: ${BASE_CMD}"
  set +e
  eval "${BASE_CMD}"
  FIX_EXIT=$?
  set -e

  if [ "${FIX_EXIT}" -ne 0 ]; then
    echo "Autofix command exited non-zero (${FIX_EXIT}), continuing to inspect generated changes"
  fi
done

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

# Capture autofix summary: file count and which fix commands ran
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

COMMIT_MSG="$(build_autofix_commit_message "${AUTOFIX_FIX_TYPES}" "${AUTOFIX_FILE_COUNT}")"

git config user.name "homeboy-ci[bot]"
git config user.email "266378653+homeboy-ci[bot]@users.noreply.github.com"
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
echo "autofix-fix-types=${AUTOFIX_FIX_TYPES}" >> "${GITHUB_OUTPUT}"
