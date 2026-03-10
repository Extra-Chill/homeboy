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
RELEASE_JSON="$(homeboy release "${COMP_ID}" ${DRY_RUN_FLAGS} --json --path "${WORKSPACE}" 2>/dev/null || true)"

# Parse the response — check for skipped_reason
SKIPPED_REASON="$(echo "${RELEASE_JSON}" | jq -r '.data.result.skipped_reason // empty' 2>/dev/null || true)"

if [ -n "${SKIPPED_REASON}" ]; then
  echo "::notice::Release skipped: ${SKIPPED_REASON}"
  {
    echo "released=false"
    echo "skipped-reason=${SKIPPED_REASON}"
  } >> "${GITHUB_OUTPUT}"

  # If major requires flag, surface the bump type for the workflow to decide
  BUMP_TYPE="$(echo "${RELEASE_JSON}" | jq -r '.data.result.bump_type // empty' 2>/dev/null || true)"
  if [ "${SKIPPED_REASON}" = "major-requires-flag" ]; then
    echo "bump-type=${BUMP_TYPE}" >> "${GITHUB_OUTPUT}"
  fi

  exit 0
fi

# Check for errors (validation failures, etc.)
SUCCESS="$(echo "${RELEASE_JSON}" | jq -r '.success // false' 2>/dev/null || true)"
if [ "${SUCCESS}" != "true" ]; then
  ERROR_MSG="$(echo "${RELEASE_JSON}" | jq -r '.error.message // "Unknown error"' 2>/dev/null || true)"
  echo "::error::Release dry-run failed: ${ERROR_MSG}"
  {
    echo "released=false"
    echo "skipped-reason=dry-run-failed"
  } >> "${GITHUB_OUTPUT}"
  exit 1
fi

# Extract planned version and bump type from dry-run
BUMP_TYPE="$(echo "${RELEASE_JSON}" | jq -r '.data.result.bump_type // empty' 2>/dev/null || true)"
NEW_VERSION="$(echo "${RELEASE_JSON}" | jq -r '.data.result.new_version // empty' 2>/dev/null || true)"
RELEASABLE="$(echo "${RELEASE_JSON}" | jq -r '.data.result.releasable_commits // 0' 2>/dev/null || true)"

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
  exit 0
fi

# --- Step 6: Run the actual release ---

RELEASE_FLAGS="--skip-checks --skip-publish --json --path ${WORKSPACE}"
if [ "${BUMP_TYPE}" = "major" ]; then
  RELEASE_FLAGS="${RELEASE_FLAGS} --major"
fi

RELEASE_RESULT="$(homeboy release "${COMP_ID}" ${RELEASE_FLAGS} 2>/dev/null || true)"

# Check success
RELEASE_SUCCESS="$(echo "${RELEASE_RESULT}" | jq -r '.success // false' 2>/dev/null || true)"
if [ "${RELEASE_SUCCESS}" != "true" ]; then
  ERROR_MSG="$(echo "${RELEASE_RESULT}" | jq -r '.error.message // "Unknown error"' 2>/dev/null || true)"
  echo "::error::Release failed: ${ERROR_MSG}"
  {
    echo "released=false"
    echo "skipped-reason=release-failed"
  } >> "${GITHUB_OUTPUT}"
  exit 1
fi

# Extract results from actual release
ACTUAL_VERSION="$(echo "${RELEASE_RESULT}" | jq -r '.data.result.new_version // empty' 2>/dev/null || true)"
ACTUAL_TAG="$(echo "${RELEASE_RESULT}" | jq -r '.data.result.tag // empty' 2>/dev/null || true)"
ACTUAL_BUMP="$(echo "${RELEASE_RESULT}" | jq -r '.data.result.bump_type // empty' 2>/dev/null || true)"

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
