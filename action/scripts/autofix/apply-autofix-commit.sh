#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

AUTOFIX_MAX_COMMITS="${AUTOFIX_MAX_COMMITS:-2}"
if ! [[ "${AUTOFIX_MAX_COMMITS}" =~ ^[0-9]+$ ]]; then
  AUTOFIX_MAX_COMMITS=2
fi

AUTOFIX_COMMIT_COUNT=$(git log --oneline --grep '^chore(ci): apply homeboy autofixes$' | wc -l | xargs)
if [ "${AUTOFIX_COMMIT_COUNT}" -ge "${AUTOFIX_MAX_COMMITS}" ]; then
  echo "Skipping autofix: reached max autofix commits (${AUTOFIX_COMMIT_COUNT}/${AUTOFIX_MAX_COMMITS})"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [ "${PR_HEAD_REPO}" != "${GITHUB_REPOSITORY}" ]; then
  echo "Skipping autofix: PR head repo (${PR_HEAD_REPO}) differs from target repo (${GITHUB_REPOSITORY})"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [ "${GITHUB_ACTOR:-}" = "github-actions[bot]" ] || [ "${GITHUB_ACTOR:-}" = "homeboy-ci[bot]" ]; then
  echo "Skipping autofix: workflow actor is ${GITHUB_ACTOR} (bot loop guard)"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

LAST_SUBJECT=$(git log -1 --pretty=%s 2>/dev/null || true)
if [ "${LAST_SUBJECT}" = "chore(ci): apply homeboy autofixes" ]; then
  echo "Skipping autofix: HEAD already an autofix commit"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [ -n "${AUTOFIX_LABEL:-}" ]; then
  if ! echo "${PR_LABELS_JSON}" | jq -e --arg label "${AUTOFIX_LABEL}" 'index($label) != null' > /dev/null; then
    echo "Skipping autofix: required label '${AUTOFIX_LABEL}' not present"
    echo "committed=false" >> "${GITHUB_OUTPUT}"
    exit 0
  fi
fi

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"

if [ -n "${AUTOFIX_COMMANDS:-}" ]; then
  IFS=',' read -ra FIX_ARRAY <<< "${AUTOFIX_COMMANDS}"
else
  FIX_ARRAY=()
  IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"
  for CMD in "${CMD_ARRAY[@]}"; do
    CMD=$(echo "${CMD}" | xargs)
    case "${CMD}" in
      lint) FIX_ARRAY+=("lint --fix") ;;
      test) FIX_ARRAY+=("test --fix") ;;
    esac
  done
fi

if [ ${#FIX_ARRAY[@]} -eq 0 ]; then
  echo "No autofix commands configured for this command set"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

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

# Update baseline for changed files so it stays current when this commit merges.
# Uses --changed-since to scope the update to PR files only, preventing
# CI/local environment parity from causing baseline churn on untouched files.
# Falls back to full baseline if HOMEBOY_CHANGED_SINCE is not set.
echo "Updating audit baseline..."
set +e
BASELINE_CMD="homeboy audit ${COMP_ID} --baseline --path ${WORKSPACE}"
if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
  BASELINE_CMD="${BASELINE_CMD} --changed-since ${HOMEBOY_CHANGED_SINCE}"
  echo "Scoped baseline update (--changed-since ${HOMEBOY_CHANGED_SINCE})"
else
  echo "Full baseline update (no --changed-since available)"
fi
eval "${BASELINE_CMD}"
BASELINE_EXIT=$?
set -e
if [ "${BASELINE_EXIT}" -ne 0 ]; then
  echo "Baseline update exited non-zero (${BASELINE_EXIT}), continuing"
fi

if git diff --quiet && git diff --cached --quiet; then
  echo "No autofix changes detected"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

git add -A
if git diff --cached --quiet; then
  echo "No staged changes after autofix"
  echo "committed=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

git config user.name "homeboy-ci[bot]"
git config user.email "266378653+homeboy-ci[bot]@users.noreply.github.com"
git commit -m "chore(ci): apply homeboy autofixes"

# Use GitHub App token for push if available — pushes from a GitHub App
# trigger workflow re-runs, while GITHUB_TOKEN pushes do not.
if [ -n "${APP_TOKEN:-}" ]; then
  echo "Pushing with GitHub App token (will trigger CI re-run)"
  git -c "http.https://github.com/.extraheader=" \
    push "https://x-access-token:${APP_TOKEN}@github.com/${GITHUB_REPOSITORY}.git" \
    "HEAD:${GITHUB_HEAD_REF}"
else
  echo "Pushing with default token (will NOT trigger CI re-run)"
  git push origin "HEAD:${GITHUB_HEAD_REF}"
fi

echo "committed=true" >> "${GITHUB_OUTPUT}"
