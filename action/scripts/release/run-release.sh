#!/usr/bin/env bash
#
# CI-driven continuous release pipeline.
#
# Delegates entirely to `homeboy release` — bump type, changelog generation,
# version bumping, commit, tag, and push are all handled by homeboy core.
#
# Env vars:
#   RELEASE_BRANCH      — branch to release from (default: main)
#   COMPONENT_NAME      — component ID override
#   RELEASE_DRY_RUN     — if "true", preview without making changes
#
# Outputs (GITHUB_OUTPUT):
#   released:        true|false
#   release-version: the version (e.g. 0.63.0)
#   release-tag:     the git tag (e.g. v0.63.0)
#   bump-type:       patch|minor|major
#   skipped-reason:  why release was skipped (if released=false)
#

set -euo pipefail

WORKSPACE="${GITHUB_WORKSPACE:-.}"
RELEASE_BRANCH="${RELEASE_BRANCH:-main}"
DRY_RUN="${RELEASE_DRY_RUN:-false}"

json_field() {
  local file_path="$1"
  local jq_expr="$2"
  jq -r "${jq_expr}" "${file_path}" 2>/dev/null || true
}

run_release_command() {
  local output_file="$1"
  shift
  local exit_code=0
  set +e
  homeboy release "$@" --output "${output_file}"
  exit_code=$?
  set -e
  if [ ! -s "${output_file}" ]; then
    echo "::warning::homeboy release did not write structured output to ${output_file}"
  fi
  return "${exit_code}"
}

# --- Step 1: Resolve component ID ---

COMP_ID="${COMPONENT_NAME:-}"
if [ -z "${COMP_ID}" ]; then
  if [ -f "${WORKSPACE}/homeboy.json" ]; then
    COMP_ID="$(jq -r '.id // empty' "${WORKSPACE}/homeboy.json" 2>/dev/null || true)"
  fi
  if [ -z "${COMP_ID}" ]; then
    COMP_ID="$(basename "${GITHUB_REPOSITORY:-unknown}")"
  fi
fi

# --- Step 2: Validate branch ---

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "${CURRENT_BRANCH}" != "${RELEASE_BRANCH}" ]; then
  echo "::notice::Not on ${RELEASE_BRANCH} (current: ${CURRENT_BRANCH}) — skipping release"
  {
    echo "released=false"
    echo "skipped-reason=wrong-branch"
  } >> "${GITHUB_OUTPUT}"
  exit 0
fi

# --- Step 2b: Sync with remote ---
# The quality gate runs in separate jobs that may push autofix commits
# or new PRs may merge while the pipeline is in flight. Pull to ensure
# we release from the actual HEAD of the branch.

git pull --ff-only origin "${RELEASE_BRANCH}" 2>/dev/null || true

# --- Step 3: Check for releasable commits via homeboy ---

DRY_RUN_FLAGS="--dry-run --skip-checks --skip-publish"
DRY_RUN_OUTPUT_FILE="$(mktemp)"
run_release_command "${DRY_RUN_OUTPUT_FILE}" "${COMP_ID}" ${DRY_RUN_FLAGS} --path "${WORKSPACE}" || true

# Parse the response — check for skipped_reason
SKIPPED_REASON="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.data.result.skipped_reason // empty')"

if [ -n "${SKIPPED_REASON}" ]; then
  echo "::notice::Release skipped: ${SKIPPED_REASON}"
  {
    echo "released=false"
    echo "skipped-reason=${SKIPPED_REASON}"
  } >> "${GITHUB_OUTPUT}"

  # If major requires flag, surface the bump type for the workflow to decide
  BUMP_TYPE="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.data.result.bump_type // empty')"
  if [ "${SKIPPED_REASON}" = "major-requires-flag" ]; then
    echo "bump-type=${BUMP_TYPE}" >> "${GITHUB_OUTPUT}"
  fi

  rm -f "${DRY_RUN_OUTPUT_FILE}"
  exit 0
fi

# Check for errors (validation failures, etc.)
SUCCESS="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.success // false')"
if [ "${SUCCESS}" != "true" ]; then
  ERROR_MSG="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.error.message // "Unknown error"')"
  if [ -z "${ERROR_MSG}" ] || [ "${ERROR_MSG}" = "null" ]; then
    ERROR_MSG="Unknown error"
  fi
  echo "::error::Release dry-run failed: ${ERROR_MSG}"
  {
    echo "released=false"
    echo "skipped-reason=dry-run-failed"
  } >> "${GITHUB_OUTPUT}"
  rm -f "${DRY_RUN_OUTPUT_FILE}"
  exit 1
fi

# Extract planned version and bump type from dry-run
BUMP_TYPE="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.data.result.bump_type // empty')"
NEW_VERSION="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.data.result.new_version // empty')"
RELEASABLE="$(json_field "${DRY_RUN_OUTPUT_FILE}" '.data.result.releasable_commits // 0')"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Release: ${COMP_ID}"
echo "  Releasable commits: ${RELEASABLE}"
echo "  Bump type: ${BUMP_TYPE}"
echo "  New version: ${NEW_VERSION}"
echo "  Dry run: ${DRY_RUN}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# --- Step 4: Configure git identity ---

BOT_NAME="homeboy-ci[bot]"
BOT_EMAIL="266378653+homeboy-ci[bot]@users.noreply.github.com"
git config user.name "${BOT_NAME}"
git config user.email "${BOT_EMAIL}"
export GIT_AUTHOR_NAME="${BOT_NAME}"
export GIT_AUTHOR_EMAIL="${BOT_EMAIL}"
export GIT_COMMITTER_NAME="${BOT_NAME}"
export GIT_COMMITTER_EMAIL="${BOT_EMAIL}"

if [ -n "${GH_TOKEN:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ]; then
  REMOTE_URL="https://x-access-token:${GH_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"
  git -c "http.https://github.com/.extraheader=" remote set-url origin "${REMOTE_URL}"
  git -c "http.https://github.com/.extraheader=" remote set-url --push origin "${REMOTE_URL}"
fi

# --- Step 5: Dry run check ---

if [ "${DRY_RUN}" = "true" ]; then
  echo "::notice::Dry run — would release v${NEW_VERSION} (${BUMP_TYPE})"
  {
    echo "released=false"
    echo "release-version=${NEW_VERSION}"
    echo "release-tag=v${NEW_VERSION}"
    echo "bump-type=${BUMP_TYPE}"
    echo "skipped-reason=dry-run"
  } >> "${GITHUB_OUTPUT}"
  rm -f "${DRY_RUN_OUTPUT_FILE}"
  exit 0
fi

# --- Step 6: Run the actual release ---

RELEASE_FLAGS="--skip-checks --skip-publish --path ${WORKSPACE}"
if [ "${BUMP_TYPE}" = "major" ]; then
  RELEASE_FLAGS="${RELEASE_FLAGS} --major"
fi

RELEASE_OUTPUT_FILE="$(mktemp)"
run_release_command "${RELEASE_OUTPUT_FILE}" "${COMP_ID}" ${RELEASE_FLAGS} || true

# Check success
RELEASE_SUCCESS="$(json_field "${RELEASE_OUTPUT_FILE}" '.success // false')"
if [ "${RELEASE_SUCCESS}" != "true" ]; then
  ERROR_MSG="$(json_field "${RELEASE_OUTPUT_FILE}" '.error.message // "Unknown error"')"
  if [ -z "${ERROR_MSG}" ] || [ "${ERROR_MSG}" = "null" ]; then
    ERROR_MSG="Unknown error"
  fi
  echo "::error::Release failed: ${ERROR_MSG}"
  {
    echo "released=false"
    echo "skipped-reason=release-failed"
  } >> "${GITHUB_OUTPUT}"
  rm -f "${DRY_RUN_OUTPUT_FILE}" "${RELEASE_OUTPUT_FILE}"
  exit 1
fi

# Extract results from actual release
ACTUAL_VERSION="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.new_version // empty')"
ACTUAL_TAG="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.tag // empty')"
ACTUAL_BUMP="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.bump_type // empty')"

# Fallback to dry-run values if extraction from run fails
ACTUAL_VERSION="${ACTUAL_VERSION:-${NEW_VERSION}}"
ACTUAL_TAG="${ACTUAL_TAG:-v${NEW_VERSION}}"
ACTUAL_BUMP="${ACTUAL_BUMP:-${BUMP_TYPE}}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Released v${ACTUAL_VERSION} (${ACTUAL_BUMP})"
echo "  Tag ${ACTUAL_TAG} pushed — build/publish workflow will trigger"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

{
  echo "released=true"
  echo "release-version=${ACTUAL_VERSION}"
  echo "release-tag=${ACTUAL_TAG}"
  echo "bump-type=${ACTUAL_BUMP}"
} >> "${GITHUB_OUTPUT}"

rm -f "${DRY_RUN_OUTPUT_FILE}" "${RELEASE_OUTPUT_FILE}"
