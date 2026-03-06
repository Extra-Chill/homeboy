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

if [ "${GITHUB_ACTOR:-}" = "github-actions[bot]" ]; then
  echo "Skipping autofix: workflow actor is github-actions[bot]"
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
  BASE_CMD="homeboy ${FIX_CMD} ${COMP_ID} --path ${WORKSPACE}"

  if echo "${FIX_CMD}" | grep -q '^lint' && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
    BASE_CMD="homeboy ${FIX_CMD} ${COMP_ID} --path ${WORKSPACE} --changed-since ${HOMEBOY_CHANGED_SINCE}"
  fi

  if [ -n "${EXTRA_ARGS:-}" ]; then
    BASE_CMD="${BASE_CMD} ${EXTRA_ARGS}"
  fi

  echo "Running autofix: ${BASE_CMD}"
  set +e
  eval "${BASE_CMD}"
  FIX_EXIT=$?
  set -e

  if [ "${FIX_EXIT}" -ne 0 ]; then
    echo "Autofix command exited non-zero (${FIX_EXIT}), continuing to check for file changes"
  fi
done

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

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git commit -m "chore(ci): apply homeboy autofixes"
git push origin "HEAD:${GITHUB_HEAD_REF}"

echo "committed=true" >> "${GITHUB_OUTPUT}"
