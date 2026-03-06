#!/usr/bin/env bash
#
# CI-driven continuous release pipeline.
#
# Called by the release workflow (cron or manual dispatch).
# Fully automatic — no human input needed.
#
# Flow:
#   1. Check for releasable commits since last tag
#   2. Compute version bump from conventional commits (fix→patch, feat→minor, breaking→major)
#   3. Generate changelog entries from commits
#   4. Bump version targets (Cargo.toml, package.json, VERSION, etc.)
#   5. Finalize changelog ([Next] → [VERSION] - DATE)
#   6. Commit version bump + changelog
#   7. Create version tag
#   8. Push commit + tag (tag push triggers build/publish workflow)
#
# Env vars:
#   RELEASE_BRANCH      — branch to release from (default: main)
#   COMPONENT_NAME      — component ID override
#   RELEASE_SKIP_CHANGELOG — if "true", skip auto-generating changelog from commits
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
SKIP_CHANGELOG="${RELEASE_SKIP_CHANGELOG:-false}"
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

# --- Step 3: Find last release tag and check for releasable commits ---

LAST_TAG="$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || echo "")"

if [ -z "${LAST_TAG}" ]; then
  echo "::notice::No previous release tag found — scanning all commits"
  COMMIT_RANGE="HEAD"
else
  COMMIT_RANGE="${LAST_TAG}..HEAD"
fi

# Parse conventional commits to determine bump type
declare -A TYPE_MAP
TYPE_MAP=(
  ["feat"]="minor"
  ["fix"]="patch"
  ["refactor"]="patch"
  ["perf"]="patch"
)

BUMP_TYPE="none"
HAS_BREAKING=false
RELEASABLE_COUNT=0

COMMIT_RE='^[a-f0-9]+ ([a-z]+)(\([^)]*\))?(!)?: (.+)$'

while IFS= read -r line; do
  [ -z "${line}" ] && continue

  # Skip release commits and merge commits
  if [[ "${line}" =~ ^[a-f0-9]+\ release: ]] || [[ "${line}" =~ ^[a-f0-9]+\ Merge\ pull\ request ]]; then
    continue
  fi

  if [[ "${line}" =~ ${COMMIT_RE} ]]; then
    TYPE="${BASH_REMATCH[1]}"
    BREAKING="${BASH_REMATCH[3]:-}"

    if [ "${BREAKING}" = "!" ]; then
      HAS_BREAKING=true
    fi

    # Skip non-releasable types
    if [[ "${TYPE}" =~ ^(chore|ci|test|style|build|docs)$ ]]; then
      continue
    fi

    COMMIT_BUMP="${TYPE_MAP[${TYPE}]:-}"
    if [ -z "${COMMIT_BUMP}" ]; then
      continue
    fi

    RELEASABLE_COUNT=$((RELEASABLE_COUNT + 1))

    # Promote bump type (patch < minor < major)
    if [ "${COMMIT_BUMP}" = "minor" ] && [ "${BUMP_TYPE}" != "major" ]; then
      BUMP_TYPE="minor"
    elif [ "${BUMP_TYPE}" = "none" ]; then
      BUMP_TYPE="${COMMIT_BUMP}"
    fi
  fi
done < <(git log "${COMMIT_RANGE}" --oneline --no-merges 2>/dev/null || true)

# Check for BREAKING CHANGE in commit bodies
if [ "${HAS_BREAKING}" = false ]; then
  if git log "${COMMIT_RANGE}" --format="%B" 2>/dev/null | grep -q "^BREAKING CHANGE:"; then
    HAS_BREAKING=true
  fi
fi

if [ "${HAS_BREAKING}" = true ]; then
  BUMP_TYPE="major"
fi

if [ "${RELEASABLE_COUNT}" -eq 0 ] || [ "${BUMP_TYPE}" = "none" ]; then
  echo "::notice::No releasable commits since ${LAST_TAG:-initial} — nothing to release"
  {
    echo "released=false"
    echo "skipped-reason=no-releasable-commits"
  } >> "${GITHUB_OUTPUT}"
  exit 0
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Release: ${COMP_ID}"
echo "  Commits since ${LAST_TAG:-initial}: ${RELEASABLE_COUNT} releasable"
echo "  Bump type: ${BUMP_TYPE}"
echo "  Dry run: ${DRY_RUN}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# --- Step 4: Configure git identity ---

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

# --- Step 5: Generate changelog from conventional commits ---

if [ "${SKIP_CHANGELOG}" != "true" ]; then
  echo "Generating changelog from conventional commits..."

  CHANGELOG_GEN_OUTPUT="$(mktemp)"
  export CHANGELOG_GEN_OUTPUT

  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  bash "${SCRIPT_DIR}/generate-changelog-from-commits.sh"

  GENERATED_COUNT="$(grep '^commit-count=' "${CHANGELOG_GEN_OUTPUT}" | cut -d= -f2)"
  rm -f "${CHANGELOG_GEN_OUTPUT}"

  if [ "${GENERATED_COUNT:-0}" != "0" ]; then
    echo "::notice::Generated changelog from ${GENERATED_COUNT} conventional commits"
  fi
fi

# --- Step 6: Read current version and compute new version ---

CURRENT_VERSION=""
if command -v homeboy &> /dev/null; then
  CURRENT_VERSION="$(homeboy version show "${COMP_ID}" --path "${WORKSPACE}" 2>/dev/null | jq -r '.data.version // empty' 2>/dev/null || true)"
fi

if [ -z "${CURRENT_VERSION}" ]; then
  if [ -f "${WORKSPACE}/Cargo.toml" ]; then
    CURRENT_VERSION="$(grep -m1 '^version' "${WORKSPACE}/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/' || true)"
  fi
fi

if [ -z "${CURRENT_VERSION}" ]; then
  echo "::error::Cannot determine current version"
  echo "released=false" >> "${GITHUB_OUTPUT}"
  exit 1
fi

# Compute new version using semver increment
IFS='.' read -r MAJOR MINOR PATCH <<< "${CURRENT_VERSION}"

case "${BUMP_TYPE}" in
  major)
    NEW_VERSION="$((MAJOR + 1)).0.0"
    ;;
  minor)
    NEW_VERSION="${MAJOR}.$((MINOR + 1)).0"
    ;;
  patch)
    NEW_VERSION="${MAJOR}.${MINOR}.$((PATCH + 1))"
    ;;
  *)
    echo "::error::Invalid bump type: ${BUMP_TYPE}"
    echo "released=false" >> "${GITHUB_OUTPUT}"
    exit 1
    ;;
esac

echo "Version: ${CURRENT_VERSION} → ${NEW_VERSION} (${BUMP_TYPE})"

# --- Step 7: Bump version targets ---

if [ -f "${WORKSPACE}/Cargo.toml" ]; then
  sed -i "s/^version = \"${CURRENT_VERSION}\"/version = \"${NEW_VERSION}\"/" "${WORKSPACE}/Cargo.toml"
  echo "  Bumped Cargo.toml"

  if [ -f "${WORKSPACE}/Cargo.lock" ]; then
    (cd "${WORKSPACE}" && cargo update --workspace 2>/dev/null || true)
  fi
fi

if [ -f "${WORKSPACE}/VERSION" ]; then
  echo "${NEW_VERSION}" > "${WORKSPACE}/VERSION"
  echo "  Bumped VERSION file"
fi

if [ -f "${WORKSPACE}/package.json" ]; then
  jq --arg v "${NEW_VERSION}" '.version = $v' "${WORKSPACE}/package.json" > "${WORKSPACE}/package.json.tmp"
  mv "${WORKSPACE}/package.json.tmp" "${WORKSPACE}/package.json"
  echo "  Bumped package.json"
fi

# Also bump via homeboy.json version targets (generic)
if [ -f "${WORKSPACE}/homeboy.json" ]; then
  VERSION_TARGETS="$(jq -r '.version.targets[]?.file // empty' "${WORKSPACE}/homeboy.json" 2>/dev/null || true)"
  if [ -n "${VERSION_TARGETS}" ]; then
    while IFS= read -r target_file; do
      [ -z "${target_file}" ] && continue
      TARGET_PATH="${WORKSPACE}/${target_file}"
      if [ -f "${TARGET_PATH}" ] && grep -q "${CURRENT_VERSION}" "${TARGET_PATH}"; then
        sed -i "s/${CURRENT_VERSION}/${NEW_VERSION}/g" "${TARGET_PATH}"
        echo "  Bumped ${target_file}"
      fi
    done <<< "${VERSION_TARGETS}"
  fi
fi

# --- Step 8: Finalize changelog ---

CHANGELOG_FILE=""
if [ -f "${WORKSPACE}/homeboy.json" ]; then
  CHANGELOG_FILE="$(jq -r '.changelog_target // empty' "${WORKSPACE}/homeboy.json" 2>/dev/null || true)"
fi
if [ -z "${CHANGELOG_FILE}" ]; then
  for candidate in "docs/changelog.md" "docs/CHANGELOG.md" "CHANGELOG.md" "changelog.md"; do
    if [ -f "${WORKSPACE}/${candidate}" ]; then
      CHANGELOG_FILE="${candidate}"
      break
    fi
  done
fi

if [ -n "${CHANGELOG_FILE}" ] && [ -f "${WORKSPACE}/${CHANGELOG_FILE}" ]; then
  TODAY="$(date -u +%Y-%m-%d)"
  sed -i -E "s/^## \[?(Next|Unreleased)\]?.*$/## [${NEW_VERSION}] - ${TODAY}/" "${WORKSPACE}/${CHANGELOG_FILE}"
  echo "  Finalized changelog: [Next] → [${NEW_VERSION}] - ${TODAY}"
fi

# --- Step 9: Dry run check ---

if [ "${DRY_RUN}" = "true" ]; then
  echo ""
  echo "::notice::Dry run — would release v${NEW_VERSION}"
  git diff --stat
  {
    echo "released=false"
    echo "release-version=${NEW_VERSION}"
    echo "release-tag=v${NEW_VERSION}"
    echo "bump-type=${BUMP_TYPE}"
    echo "skipped-reason=dry-run"
  } >> "${GITHUB_OUTPUT}"
  exit 0
fi

# --- Step 10: Commit, tag, push ---

echo ""
echo "Committing release v${NEW_VERSION}..."
git add -A
git commit -m "release: v${NEW_VERSION}

Automated by CI from conventional commits (${BUMP_TYPE} bump).
${RELEASABLE_COUNT} releasable commit(s) since ${LAST_TAG:-initial}."

RELEASE_TAG="v${NEW_VERSION}"
echo "Creating tag ${RELEASE_TAG}..."
git tag -a "${RELEASE_TAG}" -m "Release ${RELEASE_TAG}"

echo "Pushing to ${RELEASE_BRANCH}..."
git push origin "${RELEASE_BRANCH}" --follow-tags

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Released v${NEW_VERSION} (${BUMP_TYPE})"
echo "  Tag ${RELEASE_TAG} pushed — build/publish workflow will trigger"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

{
  echo "released=true"
  echo "release-version=${NEW_VERSION}"
  echo "release-tag=${RELEASE_TAG}"
  echo "bump-type=${BUMP_TYPE}"
} >> "${GITHUB_OUTPUT}"
