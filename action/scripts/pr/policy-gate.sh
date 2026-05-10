#!/usr/bin/env bash

set -euo pipefail

POLICY_PATH="${POLICY_PATH:-}"
POLICY_MERGE="${POLICY_MERGE:-false}"
POLICY_MERGE_METHOD="${POLICY_MERGE_METHOD:-squash}"
PR_NUMBER="${PR_NUMBER:-}"
REPO="${REPOSITORY:-${GITHUB_REPOSITORY:-}}"
PR_AUTHOR="${PR_AUTHOR:-}"
PR_HEAD_REF="${PR_HEAD_REF:-}"
PR_HEAD_REPO="${PR_HEAD_REPO:-}"

write_output() {
  local name="$1"
  local value="$2"

  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
      printf '%s<<HOMEBOY_PR_POLICY\n' "${name}"
      printf '%s\n' "${value}"
      printf 'HOMEBOY_PR_POLICY\n'
    } >> "${GITHUB_OUTPUT}"
  fi
}

finish() {
  local safe="$1"
  local merged="$2"
  local reason="$3"
  local report="$4"

  write_output "safe" "${safe}"
  write_output "merged" "${merged}"
  write_output "reason" "${reason}"
  write_output "report" "${report}"
  printf '%s\n' "${report}"
}

fail_closed() {
  local reason="$1"
  finish "false" "false" "${reason}" "$(printf '## PR policy\n\nUnsafe for auto-merge: %s' "${reason}")"
  exit 0
}

json_array_lines() {
  local json="$1"
  local selector="$2"
  printf '%s' "${json}" | jq -r "${selector} // [] | .[]" 2>/dev/null || true
}

json_bool() {
  local json="$1"
  local selector="$2"
  local default_value="$3"
  printf '%s' "${json}" | jq -r "${selector} // ${default_value}" 2>/dev/null || printf '%s' "${default_value}"
}

json_string() {
  local json="$1"
  local selector="$2"
  local default_value="$3"
  printf '%s' "${json}" | jq -r "${selector} // \"${default_value}\"" 2>/dev/null || printf '%s' "${default_value}"
}

load_policy_json() {
  local path="$1"

  if [ ! -f "${path}" ]; then
    fail_closed "policy file not found: ${path}"
  fi

  if jq -e . "${path}" >/dev/null 2>&1; then
    jq -c . "${path}"
    return
  fi

  if command -v ruby >/dev/null 2>&1; then
    ruby -ryaml -rjson -e 'puts JSON.generate(YAML.load_file(ARGV.fetch(0)) || {})' "${path}" 2>/dev/null && return
  fi

  fail_closed "policy file is not valid JSON and Ruby YAML support is unavailable"
}

matches_pattern() {
  local value="$1"
  local pattern="$2"

  if [ "${value}" = "${pattern}" ]; then
    return 0
  fi

  case "${value}" in
    ${pattern}) return 0 ;;
    *) return 1 ;;
  esac
}

matches_any_pattern() {
  local value="$1"
  shift

  local pattern
  for pattern in "$@"; do
    if matches_pattern "${value}" "${pattern}"; then
      return 0
    fi
  done

  return 1
}

join_lines() {
  local joined=""
  local line

  while IFS= read -r line; do
    [ -n "${line}" ] || continue
    if [ -n "${joined}" ]; then
      joined="${joined}, ${line}"
    else
      joined="${line}"
    fi
  done

  printf '%s' "${joined}"
}

if [ -z "${POLICY_PATH}" ]; then
  fail_closed "POLICY_PATH is required"
fi

if [ -z "${PR_NUMBER}" ] || [ -z "${REPO}" ]; then
  fail_closed "PR_NUMBER and repository are required"
fi

if ! command -v jq >/dev/null 2>&1; then
  fail_closed "jq is required"
fi

if ! command -v gh >/dev/null 2>&1; then
  fail_closed "gh is required"
fi

POLICY_JSON="$(load_policy_json "${POLICY_PATH}")"

mapfile -t ALLOWED_AUTHORS < <(json_array_lines "${POLICY_JSON}" '.allowed_authors')
mapfile -t ALLOWED_HEAD_BRANCHES < <(json_array_lines "${POLICY_JSON}" '.allowed_head_branches')
mapfile -t ALLOWED_HEAD_REPOS < <(json_array_lines "${POLICY_JSON}" '.allowed_head_repositories')
mapfile -t ALLOWED_PATHS < <(json_array_lines "${POLICY_JSON}" '.allowed_paths')
mapfile -t BLOCKED_PATHS < <(json_array_lines "${POLICY_JSON}" '.blocked_paths')
mapfile -t BLOCKED_CONTENT < <(json_array_lines "${POLICY_JSON}" '.blocked_content_patterns')

REQUIRE_SAME_REPO="$(json_bool "${POLICY_JSON}" '.require_same_repository' 'true')"
DELETE_BRANCH="$(json_bool "${POLICY_JSON}" '.delete_branch_on_merge' 'true')"
POLICY_TITLE="$(json_string "${POLICY_JSON}" '.title' 'PR policy')"

if [ "${REQUIRE_SAME_REPO}" = "true" ] && [ -n "${PR_HEAD_REPO}" ] && [ "${PR_HEAD_REPO}" != "${REPO}" ]; then
  fail_closed "head repository ${PR_HEAD_REPO} does not match ${REPO}"
fi

if [ "${#ALLOWED_AUTHORS[@]}" -gt 0 ] && ! matches_any_pattern "${PR_AUTHOR}" "${ALLOWED_AUTHORS[@]}"; then
  fail_closed "author ${PR_AUTHOR:-unknown} is not allowed"
fi

if [ "${#ALLOWED_HEAD_REPOS[@]}" -gt 0 ] && ! matches_any_pattern "${PR_HEAD_REPO}" "${ALLOWED_HEAD_REPOS[@]}"; then
  fail_closed "head repository ${PR_HEAD_REPO:-unknown} is not allowed"
fi

if [ "${#ALLOWED_HEAD_BRANCHES[@]}" -gt 0 ] && ! matches_any_pattern "${PR_HEAD_REF}" "${ALLOWED_HEAD_BRANCHES[@]}"; then
  fail_closed "head branch ${PR_HEAD_REF:-unknown} is not allowed"
fi

FILES="$(gh api --paginate "repos/${REPO}/pulls/${PR_NUMBER}/files" --jq '.[].filename' 2>/dev/null || true)"
if [ -z "${FILES}" ]; then
  fail_closed "could not read changed files"
fi

UNALLOWED_FILES=()
BLOCKED_FILES=()
while IFS= read -r file; do
  [ -n "${file}" ] || continue

  if [ "${#BLOCKED_PATHS[@]}" -gt 0 ] && matches_any_pattern "${file}" "${BLOCKED_PATHS[@]}"; then
    BLOCKED_FILES+=("${file}")
  fi

  if [ "${#ALLOWED_PATHS[@]}" -gt 0 ] && ! matches_any_pattern "${file}" "${ALLOWED_PATHS[@]}"; then
    UNALLOWED_FILES+=("${file}")
  fi
done <<< "${FILES}"

if [ "${#BLOCKED_FILES[@]}" -gt 0 ]; then
  fail_closed "blocked paths changed: $(printf '%s\n' "${BLOCKED_FILES[@]}" | join_lines)"
fi

if [ "${#UNALLOWED_FILES[@]}" -gt 0 ]; then
  fail_closed "paths outside allowed set changed: $(printf '%s\n' "${UNALLOWED_FILES[@]}" | join_lines)"
fi

CONTENT_HITS=()
if [ "${#BLOCKED_CONTENT[@]}" -gt 0 ]; then
  BLOCKED_CONTENT_PATTERN="$(printf '%s\n' "${BLOCKED_CONTENT[@]}" | paste -sd '|' -)"

  while IFS= read -r file; do
    [ -n "${file}" ] || continue
    [ -f "${file}" ] || continue

    if grep -Eq "${BLOCKED_CONTENT_PATTERN}" "${file}"; then
      CONTENT_HITS+=("${file}")
    fi
  done <<< "${FILES}"
fi

if [ "${#CONTENT_HITS[@]}" -gt 0 ]; then
  fail_closed "blocked content patterns found in: $(printf '%s\n' "${CONTENT_HITS[@]}" | join_lines)"
fi

MERGED="false"
MERGE_NOTE="Merge was not requested."
if [ "${POLICY_MERGE}" = "true" ]; then
  case "${POLICY_MERGE_METHOD}" in
    merge|squash|rebase) ;;
    *) fail_closed "unsupported merge method: ${POLICY_MERGE_METHOD}" ;;
  esac

  MERGE_ARGS=("pr" "merge" "${PR_NUMBER}" "--repo" "${REPO}" "--${POLICY_MERGE_METHOD}")
  if [ "${DELETE_BRANCH}" = "true" ]; then
    MERGE_ARGS+=("--delete-branch")
  fi

  gh "${MERGE_ARGS[@]}"
  MERGED="true"
  MERGE_NOTE="Merged with ${POLICY_MERGE_METHOD}."
fi

FILE_COUNT="$(printf '%s\n' "${FILES}" | sed '/^$/d' | wc -l | tr -d ' ')"
REPORT="$(printf '## %s\n\nSafe for auto-merge: %s changed file(s) matched policy. %s' "${POLICY_TITLE}" "${FILE_COUNT}" "${MERGE_NOTE}")"
finish "true" "${MERGED}" "safe" "${REPORT}"
