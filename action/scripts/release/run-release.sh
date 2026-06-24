#!/usr/bin/env bash
#
# CI-driven continuous release pipeline.
#
# Release decisions belong to `homeboy release`. This wrapper only resolves the
# GitHub Actions workspace, applies branch safety, and translates structured
# Homeboy output into composite-action outputs.
#
# Env vars:
#   RELEASE_BRANCH      - branch to release from (default: main)
#   COMPONENT_NAME      - component ID override
#   RELEASE_DRY_RUN     - if "true", preview without making changes
#   RELEASE_HEAD        - if "true", finish release at existing HEAD/tag
#   RELEASE_FROM_ARTIFACTS - artifact directory passed to --from-artifacts
#
# Outputs (GITHUB_OUTPUT):
#   released:        true|false
#   release-version: the version (e.g. 0.63.0)
#   release-tag:     the git tag (e.g. v0.63.0)
#   release-bump-type: patch|minor|major
#   skipped-reason:  why release was skipped (if released=false)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/github-release.sh"

RELEASE_BRANCH="${RELEASE_BRANCH:-main}"
DRY_RUN="${RELEASE_DRY_RUN:-false}"
RELEASE_HEAD="${RELEASE_HEAD:-false}"
RELEASE_FROM_ARTIFACTS="${RELEASE_FROM_ARTIFACTS:-}"

COMPONENT_DIR="${COMPONENT_DIR:-.}"
if [ -n "${COMPONENT_DIR}" ] && [ "${COMPONENT_DIR}" != "." ]; then
  WORKSPACE="${GITHUB_WORKSPACE:-.}/${COMPONENT_DIR}"
else
  WORKSPACE="${GITHUB_WORKSPACE:-.}"
fi

json_field() {
  local file_path="$1"
  local jq_expr="$2"
  jq -r "${jq_expr}" "${file_path}" 2>/dev/null || true
}

write_output() {
  local key="$1"
  local value="$2"

  echo "${key}=${value}" >> "${GITHUB_OUTPUT}"
}

configure_git_push_auth() {
  if [ -z "${GH_TOKEN:-}" ]; then
    return 0
  fi

  GIT_ASKPASS_SCRIPT="$(mktemp)"
  chmod 700 "${GIT_ASKPASS_SCRIPT}"
  cat > "${GIT_ASKPASS_SCRIPT}" <<'SH'
#!/usr/bin/env bash
case "$1" in
  *Username*) printf '%s\n' 'x-access-token' ;;
  *Password*) printf '%s\n' "${GH_TOKEN}" ;;
  *) printf '\n' ;;
esac
SH

  export GIT_ASKPASS="${GIT_ASKPASS_SCRIPT}"
  export GIT_TERMINAL_PROMPT=0
  trap 'rm -f "${GIT_ASKPASS_SCRIPT:-}"' EXIT
}

resolve_component_id() {
  if [ -n "${COMPONENT_NAME:-}" ]; then
    echo "${COMPONENT_NAME}"
    return 0
  fi

  if [ -f "${WORKSPACE}/homeboy.json" ]; then
    local configured_id
    configured_id="$(jq -r '.id // empty' "${WORKSPACE}/homeboy.json" 2>/dev/null || true)"
    if [ -n "${configured_id}" ]; then
      echo "${configured_id}"
      return 0
    fi
  fi

  basename "${GITHUB_REPOSITORY:-unknown}"
}

write_release_outputs() {
  local output_file="$1"
  local released="$2"
  local skipped_reason="${3:-}"

  local version tag bump_type
  version="$(json_field "${output_file}" '.data.result.new_version // empty')"
  tag="$(json_field "${output_file}" '.data.result.tag // empty')"
  bump_type="$(json_field "${output_file}" '.data.result.bump_type // empty')"

  write_output "released" "${released}"
  [ -n "${version}" ] && write_output "release-version" "${version}"
  [ -n "${tag}" ] && write_output "release-tag" "${tag}"
  if [ -n "${bump_type}" ]; then
    write_output "release-bump-type" "${bump_type}"
    write_output "bump-type" "${bump_type}"
  fi
  [ -n "${skipped_reason}" ] && write_output "skipped-reason" "${skipped_reason}"

  return 0
}

COMP_ID="$(resolve_component_id)"

configure_git_push_auth

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "${RELEASE_HEAD}" != "true" ] && [ "${CURRENT_BRANCH}" != "${RELEASE_BRANCH}" ]; then
  echo "::notice::Not on ${RELEASE_BRANCH} (current: ${CURRENT_BRANCH}) - skipping release"
  write_output "released" "false"
  write_output "skipped-reason" "wrong-branch"
  exit 0
fi

DEFAULT_BRANCH="$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||' || true)"
if [ -z "${DEFAULT_BRANCH}" ]; then
  DEFAULT_BRANCH="main"
fi

if [ "${RELEASE_HEAD}" != "true" ] && [ "${CURRENT_BRANCH}" != "${DEFAULT_BRANCH}" ]; then
  echo "::error::Refusing to release from non-default branch '${CURRENT_BRANCH}' (default: '${DEFAULT_BRANCH}')"
  write_output "released" "false"
  write_output "skipped-reason" "wrong-default-branch"
  exit 1
fi

# Tip-sync handled in core: `homeboy release` runs `validate_remote_sync`
# which fetches and fast-forwards from origin before the working-tree check,
# and `get_uncommitted_changes` runs `git update-index --refresh` so any
# stale stat info (from prior cargo build / extension setup steps) does not
# surface as a false-positive dirty tree. The wrapper used to do its own
# `git pull --ff-only` here as a workaround, which was the original symptom
# of the upstream gap fixed in homeboy core. See homeboy PR #2368.

# Quality gates run in separate jobs before this script, so --skip-checks
# is always safe here. The publish and github.release flags are
# configurable: downstream consumers that wire up extensions like
# homeboy-extensions/wordpress (release.package + release.publish) want
# the full pipeline by default. Components that publish artifacts or
# create GitHub Releases outside the homeboy pipeline (binary cross-
# compile, hand-rolled gh release create, …) opt out via the action
# inputs that set these env vars.
RELEASE_SKIP_PUBLISH="${RELEASE_SKIP_PUBLISH:-false}"
RELEASE_SKIP_GITHUB_RELEASE="${RELEASE_SKIP_GITHUB_RELEASE:-false}"

RELEASE_OUTPUT_FILE="$(mktemp)"
RELEASE_ARGS=(
  "${COMP_ID}"
  --path "${WORKSPACE}"
  --skip-checks
  --git-identity bot
)

if [ "${RELEASE_SKIP_PUBLISH}" = "true" ]; then
  RELEASE_ARGS+=(--skip-publish)
fi

if [ "${RELEASE_SKIP_GITHUB_RELEASE}" = "true" ]; then
  RELEASE_ARGS+=(--no-github-release --i-know-ci-creates-the-github-release)
fi

if [ "${RELEASE_HEAD}" = "true" ]; then
  RELEASE_ARGS+=(--head)
fi

if [ -n "${RELEASE_FROM_ARTIFACTS}" ]; then
  RELEASE_ARGS+=(--from-artifacts "${RELEASE_FROM_ARTIFACTS}")
fi

if [ "${DRY_RUN}" = "true" ]; then
  RELEASE_ARGS+=(--dry-run)
else
  # Homeboy core requires a real release that bypasses quality gates with
  # --skip-checks to also pass --apply, so an unattended CI release cannot
  # silently ship while a bare --skip-checks would otherwise refuse. Quality
  # gates already ran in separate jobs before this script, so the explicit
  # opt-in is satisfied here.
  RELEASE_ARGS+=(--apply)
fi

set +e
homeboy --output "${RELEASE_OUTPUT_FILE}" release "${RELEASE_ARGS[@]}"
RELEASE_EXIT=$?
set -e

if [ ! -s "${RELEASE_OUTPUT_FILE}" ]; then
  echo "::error::homeboy release did not write structured output to ${RELEASE_OUTPUT_FILE}"
  write_output "released" "false"
  write_output "skipped-reason" "release-output-missing"
  rm -f "${RELEASE_OUTPUT_FILE}"
  exit 1
fi

SUCCESS="$(json_field "${RELEASE_OUTPUT_FILE}" '.success // false')"
SKIPPED_REASON="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.skipped_reason // empty')"

if [ -n "${SKIPPED_REASON}" ]; then
  echo "::notice::Release skipped: ${SKIPPED_REASON}"
  write_release_outputs "${RELEASE_OUTPUT_FILE}" "false" "${SKIPPED_REASON}"
  rm -f "${RELEASE_OUTPUT_FILE}"
  exit 0
fi

if [ "${SUCCESS}" != "true" ] || [ "${RELEASE_EXIT}" -ne 0 ]; then
  ERROR_MSG="$(json_field "${RELEASE_OUTPUT_FILE}" '.error.message // "Unknown error"')"
  if [ -z "${ERROR_MSG}" ] || [ "${ERROR_MSG}" = "null" ]; then
    ERROR_MSG="Unknown error"
  fi

  echo "::error::Release failed: ${ERROR_MSG}"
  write_release_outputs "${RELEASE_OUTPUT_FILE}" "false" "release-failed"
  rm -f "${RELEASE_OUTPUT_FILE}"
  exit 1
fi

VERSION="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.new_version // empty')"
TAG="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.tag // empty')"
BUMP_TYPE="$(json_field "${RELEASE_OUTPUT_FILE}" '.data.result.bump_type // empty')"

if [ "${DRY_RUN}" = "true" ]; then
  echo "::notice::Dry run - would release ${TAG:-v${VERSION}} (${BUMP_TYPE})"
  write_release_outputs "${RELEASE_OUTPUT_FILE}" "false" "dry-run"
  rm -f "${RELEASE_OUTPUT_FILE}"
  exit 0
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Released ${TAG} (${BUMP_TYPE})"
echo "  Tag pushed - build/publish workflow will trigger"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

write_release_outputs "${RELEASE_OUTPUT_FILE}" "true"
rm -f "${RELEASE_OUTPUT_FILE}"
