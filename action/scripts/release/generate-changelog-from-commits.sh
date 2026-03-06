#!/usr/bin/env bash
#
# Parse conventional commits since the last release tag and add changelog entries
# via homeboy changelog add. Also outputs the recommended bump type.
#
# Conventional commit format: type(scope): description
#   feat:     → Added   → minor
#   fix:      → Fixed   → patch
#   refactor: → Refactored → patch
#   perf:     → Changed → patch
#   BREAKING CHANGE / feat!: / fix!: → major
#
# Outputs (written to CHANGELOG_GEN_OUTPUT file if set, else GITHUB_OUTPUT):
#   recommended-bump: patch|minor|major|none
#   changelog-added:  true|false
#   commit-count:     number of releasable commits found
#

set -euo pipefail

OUTPUT_FILE="${CHANGELOG_GEN_OUTPUT:-${GITHUB_OUTPUT}}"

COMP_ID="${COMPONENT_NAME:-}"
WORKSPACE="${GITHUB_WORKSPACE:-.}"

if [ -z "${COMP_ID}" ]; then
  if [ -f "${WORKSPACE}/homeboy.json" ]; then
    COMP_ID="$(jq -r '.id // empty' "${WORKSPACE}/homeboy.json" 2>/dev/null || true)"
  fi
  if [ -z "${COMP_ID}" ]; then
    COMP_ID="$(basename "${GITHUB_REPOSITORY:-unknown}")"
  fi
fi

# Find the last release tag
LAST_TAG="$(git describe --tags --abbrev=0 2>/dev/null || echo "")"

if [ -z "${LAST_TAG}" ]; then
  echo "::notice::No previous release tag found — scanning all commits"
  COMMIT_RANGE="HEAD"
else
  echo "::notice::Generating changelog from commits since ${LAST_TAG}"
  COMMIT_RANGE="${LAST_TAG}..HEAD"
fi

# Parse commits into conventional commit types
declare -A TYPE_ENTRIES
declare -A TYPE_MAP
TYPE_MAP=(
  ["feat"]="Added"
  ["fix"]="Fixed"
  ["refactor"]="Refactored"
  ["perf"]="Changed"
)

RECOMMENDED_BUMP="none"
COMMIT_COUNT=0
HAS_BREAKING=false

# Regex for conventional commit: hash type(scope)!: message
# Stored in variable to avoid bash [[ =~ ]] quoting issues with parentheses
COMMIT_RE='^[a-f0-9]+ ([a-z]+)(\(([^)]*)\))?(!)?: (.+)$'

while IFS= read -r line; do
  [ -z "${line}" ] && continue

  # Skip release commits and merge commits
  if [[ "${line}" =~ ^[a-f0-9]+\ release: ]] || [[ "${line}" =~ ^[a-f0-9]+\ Merge\ pull\ request ]]; then
    continue
  fi

  # Extract conventional commit parts
  if [[ "${line}" =~ ${COMMIT_RE} ]]; then
    TYPE="${BASH_REMATCH[1]}"
    SCOPE="${BASH_REMATCH[3]:-}"
    BREAKING="${BASH_REMATCH[4]:-}"
    MESSAGE="${BASH_REMATCH[5]}"

    # Check for breaking change
    if [ "${BREAKING}" = "!" ]; then
      HAS_BREAKING=true
    fi

    # Skip non-user-facing types
    if [[ "${TYPE}" == "chore" ]] || [[ "${TYPE}" == "ci" ]] || [[ "${TYPE}" == "test" ]] || [[ "${TYPE}" == "style" ]] || [[ "${TYPE}" == "build" ]] || [[ "${TYPE}" == "docs" ]]; then
      continue
    fi

    # Map type to changelog section
    CHANGELOG_TYPE="${TYPE_MAP[${TYPE}]:-}"
    if [ -z "${CHANGELOG_TYPE}" ]; then
      continue
    fi

    # Build the entry message (include scope if present)
    if [ -n "${SCOPE}" ]; then
      ENTRY="${MESSAGE} (${SCOPE})"
    else
      ENTRY="${MESSAGE}"
    fi

    # Accumulate entries by type
    if [ -z "${TYPE_ENTRIES[${CHANGELOG_TYPE}]:-}" ]; then
      TYPE_ENTRIES["${CHANGELOG_TYPE}"]="${ENTRY}"
    else
      TYPE_ENTRIES["${CHANGELOG_TYPE}"]="${TYPE_ENTRIES[${CHANGELOG_TYPE}]}"$'\n'"${ENTRY}"
    fi

    COMMIT_COUNT=$((COMMIT_COUNT + 1))

    # Track bump level
    if [ "${TYPE}" = "feat" ] && [ "${RECOMMENDED_BUMP}" != "major" ]; then
      RECOMMENDED_BUMP="minor"
    elif [ "${RECOMMENDED_BUMP}" = "none" ]; then
      RECOMMENDED_BUMP="patch"
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
  RECOMMENDED_BUMP="major"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Changelog generation: ${COMMIT_COUNT} releasable commits"
echo "  Recommended bump: ${RECOMMENDED_BUMP}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

CHANGELOG_ADDED=false

if [ "${COMMIT_COUNT}" -gt 0 ]; then
  for CHANGELOG_TYPE in "Added" "Changed" "Fixed" "Refactored" "Removed" "Deprecated" "Security"; do
    ENTRIES="${TYPE_ENTRIES[${CHANGELOG_TYPE}]:-}"
    [ -z "${ENTRIES}" ] && continue

    # Build -m flags for each entry
    ARGS=()
    while IFS= read -r entry; do
      [ -z "${entry}" ] && continue
      ARGS+=(-m "${entry}")
    done <<< "${ENTRIES}"

    echo "  Adding ${CHANGELOG_TYPE} entries:"
    while IFS= read -r entry; do
      [ -z "${entry}" ] && continue
      echo "    - ${entry}"
    done <<< "${ENTRIES}"

    homeboy changelog add "${COMP_ID}" "${ARGS[@]}" --type "${CHANGELOG_TYPE}"
    CHANGELOG_ADDED=true
  done
fi

echo ""

# Output results
{
  echo "recommended-bump=${RECOMMENDED_BUMP}"
  echo "changelog-added=${CHANGELOG_ADDED}"
  echo "commit-count=${COMMIT_COUNT}"
} >> "${OUTPUT_FILE}"
