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
#
# Outputs (GITHUB_OUTPUT):
#   released:        true|false
#   release-version: the version (e.g. 0.63.0)
#   release-tag:     the git tag (e.g. v0.63.0)
#   bump-type:       patch|minor|major
#   skipped-reason:  why release was skipped (if released=false)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/github-release.sh"

RELEASE_BRANCH="${RELEASE_BRANCH:-main}"
DRY_RUN="${RELEASE_DRY_RUN:-false}"

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
  [ -n "${bump_type}" ] && write_output "bump-type" "${bump_type}"
  [ -n "${skipped_reason}" ] && write_output "skipped-reason" "${skipped_reason}"

  return 0
}

configure_release_git_auth() {
  if [ "${DRY_RUN}" = "true" ] || [ -z "${GH_TOKEN:-}" ]; then
    return 0
  fi

  local encoded_token
  encoded_token="$(printf 'x-access-token:%s' "${GH_TOKEN}" | base64 | tr -d '\n')"
  git config --local http.https://github.com/.extraheader "AUTHORIZATION: basic ${encoded_token}"
}

COMP_ID="$(resolve_component_id)"

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "${CURRENT_BRANCH}" != "${RELEASE_BRANCH}" ]; then
  echo "::notice::Not on ${RELEASE_BRANCH} (current: ${CURRENT_BRANCH}) - skipping release"
  write_output "released" "false"
  write_output "skipped-reason" "wrong-branch"
  exit 0
fi

DEFAULT_BRANCH="$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||' || true)"
if [ -z "${DEFAULT_BRANCH}" ]; then
  DEFAULT_BRANCH="main"
fi

if [ "${CURRENT_BRANCH}" != "${DEFAULT_BRANCH}" ]; then
  echo "::error::Refusing to release from non-default branch '${CURRENT_BRANCH}' (default: '${DEFAULT_BRANCH}')"
  write_output "released" "false"
  write_output "skipped-reason" "wrong-default-branch"
  exit 1
fi

# The quality gate runs in separate jobs that may push autofix commits or new
# PRs may merge while the pipeline is in flight. Pull before invoking Homeboy so
# core releases from the actual branch head.
git pull --ff-only origin "${RELEASE_BRANCH}" 2>/dev/null || true
configure_release_git_auth

RELEASE_OUTPUT_FILE="$(mktemp)"
RELEASE_ARGS=(
  "${COMP_ID}"
  --path "${WORKSPACE}"
  --skip-checks
  --skip-publish
  --git-identity bot
)

if [ "${DRY_RUN}" = "true" ]; then
  RELEASE_ARGS+=(--dry-run)
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

homeboy_verify_github_release_exists "${TAG}" "${GITHUB_REPOSITORY:-}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Released ${TAG} (${BUMP_TYPE})"
echo "  Tag pushed - build/publish workflow will trigger"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

write_release_outputs "${RELEASE_OUTPUT_FILE}" "true"
rm -f "${RELEASE_OUTPUT_FILE}"
