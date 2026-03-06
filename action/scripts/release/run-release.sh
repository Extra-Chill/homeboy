#!/usr/bin/env bash
#
# Run the full release pipeline after quality gates have passed.
#
# Flow:
#   1. Validate release conditions (on main, quality gates passed, etc.)
#   2. Generate changelog from conventional commits (if no unreleased entries exist)
#   3. Determine bump type (explicit input > commit-derived > skip)
#   4. Run homeboy release <comp> <type> --skip-checks
#
# Env vars:
#   QUALITY_RESULTS     — JSON object from quality gate commands (e.g. {"lint":"pass","test":"pass"})
#   RELEASE_BUMP_TYPE   — explicit bump type (patch/minor/major/auto), default: auto
#   RELEASE_DRY_RUN     — if "true", only preview the release
#   COMPONENT_NAME      — component ID override
#   RELEASE_SKIP_CHANGELOG — if "true", skip auto-generating changelog from commits
#
# Outputs (GITHUB_OUTPUT):
#   released:       true|false
#   release-version: the new version (e.g. 1.2.3)
#   release-tag:    the git tag (e.g. v1.2.3)
#   bump-type:      the bump type used
#

set -euo pipefail

WORKSPACE="${GITHUB_WORKSPACE:-.}"
BUMP_TYPE="${RELEASE_BUMP_TYPE:-auto}"
DRY_RUN="${RELEASE_DRY_RUN:-false}"
SKIP_CHANGELOG="${RELEASE_SKIP_CHANGELOG:-false}"

# --- Resolve component ID ---

COMP_ID="${COMPONENT_NAME:-}"
if [ -z "${COMP_ID}" ]; then
  if [ -f "${WORKSPACE}/homeboy.json" ]; then
    COMP_ID="$(jq -r '.id // empty' "${WORKSPACE}/homeboy.json" 2>/dev/null || true)"
  fi
  if [ -z "${COMP_ID}" ]; then
    COMP_ID="$(basename "${GITHUB_REPOSITORY:-unknown}")"
  fi
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Release: ${COMP_ID}"
echo "  Bump type: ${BUMP_TYPE}"
echo "  Dry run: ${DRY_RUN}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# --- Step 1: Validate quality gates ---

if [ -n "${QUALITY_RESULTS:-}" ] && [ "${QUALITY_RESULTS}" != "{}" ]; then
  if echo "${QUALITY_RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null 2>&1; then
    echo "::error::Release aborted — quality gates failed"
    echo "${QUALITY_RESULTS}" | jq -r 'to_entries[] | select(.value == "fail") | "  ✗ \(.key): FAILED"'
    echo "released=false" >> "${GITHUB_OUTPUT}"
    exit 1
  fi
  echo "Quality gates passed:"
  echo "${QUALITY_RESULTS}" | jq -r 'to_entries[] | "  ✓ \(.key): \(.value)"'
  echo ""
fi

# --- Step 2: Validate branch ---

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
RELEASE_BRANCH="${RELEASE_BRANCH:-main}"

if [ "${CURRENT_BRANCH}" != "${RELEASE_BRANCH}" ]; then
  echo "::warning::Skipping release — not on ${RELEASE_BRANCH} branch (current: ${CURRENT_BRANCH})"
  echo "released=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

# --- Step 3: Configure git identity for CI commits ---

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

# --- Step 4: Generate changelog from conventional commits ---

if [ "${SKIP_CHANGELOG}" != "true" ]; then
  # Check if unreleased changelog entries already exist
  CHANGELOG_FILE="$(jq -r '.changelog_target // "docs/CHANGELOG.md"' "${WORKSPACE}/homeboy.json" 2>/dev/null || echo "docs/CHANGELOG.md")"
  HAS_UNRELEASED=false
  if [ -f "${CHANGELOG_FILE}" ]; then
    if grep -qiE '## \[?(Unreleased|Next)\]?' "${CHANGELOG_FILE}"; then
      HAS_UNRELEASED=true
      echo "::notice::Unreleased changelog entries already exist — skipping auto-generation"
    fi
  fi

  if [ "${HAS_UNRELEASED}" = false ]; then
    echo "Generating changelog from conventional commits..."
    CHANGELOG_GEN_OUTPUT="$(mktemp)"
    export CHANGELOG_GEN_OUTPUT
    bash "${GITHUB_ACTION_PATH}/scripts/release/generate-changelog-from-commits.sh"

    GENERATED_BUMP="$(grep '^recommended-bump=' "${CHANGELOG_GEN_OUTPUT}" | cut -d= -f2)"
    GENERATED_COUNT="$(grep '^commit-count=' "${CHANGELOG_GEN_OUTPUT}" | cut -d= -f2)"
    rm -f "${CHANGELOG_GEN_OUTPUT}"

    if [ "${GENERATED_COUNT:-0}" = "0" ]; then
      echo "::notice::No releasable conventional commits found since last tag — skipping release"
      echo "released=false" >> "${GITHUB_OUTPUT}"
      exit 0
    fi

    # Use generated bump type if auto
    if [ "${BUMP_TYPE}" = "auto" ]; then
      BUMP_TYPE="${GENERATED_BUMP}"
      echo "::notice::Auto-detected bump type from commits: ${BUMP_TYPE}"
    fi
  fi
fi

# --- Step 5: Validate bump type ---

if [ "${BUMP_TYPE}" = "auto" ] || [ "${BUMP_TYPE}" = "none" ]; then
  echo "::notice::No bump type resolved — skipping release"
  echo "released=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [[ ! "${BUMP_TYPE}" =~ ^(patch|minor|major)$ ]]; then
  echo "::error::Invalid bump type: ${BUMP_TYPE} (must be patch, minor, or major)"
  echo "released=false" >> "${GITHUB_OUTPUT}"
  exit 1
fi

# --- Step 6: Run release ---

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Releasing ${COMP_ID} with ${BUMP_TYPE} bump"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

RELEASE_CMD="homeboy release ${COMP_ID} ${BUMP_TYPE} --skip-checks --path ${WORKSPACE}"

if [ "${DRY_RUN}" = "true" ]; then
  RELEASE_CMD="${RELEASE_CMD} --dry-run"
fi

echo "Running: ${RELEASE_CMD}"
RELEASE_LOG="$(mktemp)"
set +e
eval "${RELEASE_CMD}" 2>&1 | tee "${RELEASE_LOG}"
RELEASE_EXIT=${PIPESTATUS[0]}
set -e

if [ "${RELEASE_EXIT}" -ne 0 ]; then
  echo "::error::Release failed (exit code ${RELEASE_EXIT})"
  echo "released=false" >> "${GITHUB_OUTPUT}"
  rm -f "${RELEASE_LOG}"
  exit 1
fi

RELEASE_OUTPUT="$(cat "${RELEASE_LOG}")"
rm -f "${RELEASE_LOG}"

# --- Step 7: Extract version from release output ---

NEW_VERSION="$(echo "${RELEASE_OUTPUT}" | jq -r '.data.result.plan.steps[] | select(.type == "version") | .config.to // empty' 2>/dev/null || true)"

if [ -z "${NEW_VERSION}" ]; then
  # Fallback: read from VERSION file
  if [ -f "${WORKSPACE}/VERSION" ]; then
    NEW_VERSION="$(cat "${WORKSPACE}/VERSION" | tr -d '[:space:]')"
  fi
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "${DRY_RUN}" = "true" ]; then
  echo "  Dry run complete — would release v${NEW_VERSION}"
else
  echo "  Released v${NEW_VERSION} ✓"
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

{
  echo "released=true"
  echo "release-version=${NEW_VERSION}"
  echo "release-tag=v${NEW_VERSION}"
  echo "bump-type=${BUMP_TYPE}"
} >> "${GITHUB_OUTPUT}"
