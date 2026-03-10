#!/usr/bin/env bash

set -euo pipefail

# Source scope module for flag generation
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../scope/flags.sh"

# Prefix used for all autofix commits. Loop guards grep for this prefix,
# so the subject line can vary after it (e.g. fix types, file count).
AUTOFIX_COMMIT_PREFIX="chore(ci): homeboy autofix"

# Build an informative autofix commit message.
# Subject: chore(ci): homeboy autofix — audit, lint (7 files)
# Body: list of changed files for traceability.
build_autofix_commit_message() {
  local fix_types="$1"
  local file_count="$2"

  local subject="${AUTOFIX_COMMIT_PREFIX}"
  if [ -n "${fix_types}" ]; then
    subject="${subject} — ${fix_types}"
  fi
  subject="${subject} (${file_count} files)"

  local body
  body="$(git diff --cached --name-only | sort)"

  printf '%s\n\n%s\n' "${subject}" "${body}"
}

resolve_component_id() {
  if [ -n "${COMPONENT_NAME:-}" ]; then
    printf '%s\n' "${COMPONENT_NAME}"
  elif [ -n "${component_id:-}" ]; then
    printf '%s\n' "${component_id}"
  elif [ -f "homeboy.json" ]; then
    local from_portable
    from_portable="$(jq -r '.id // empty' homeboy.json 2>/dev/null || true)"
    if [ -n "${from_portable}" ]; then
      printf '%s\n' "${from_portable}"
    else
      basename "${GITHUB_REPOSITORY}"
    fi
  else
    basename "${GITHUB_REPOSITORY}"
  fi
}

resolve_workspace() {
  pwd
}

# Sort commands into canonical order: audit → lint → test → refactor.
# Audit/lint/test are read-first quality gates. Refactor is the single
# write/mutation phase when used.
canonicalize_commands() {
  local commands="$1"
  local audit="" lint="" test="" refactor="" others=()
  local cmd base_cmd

  IFS=',' read -ra CMD_ARRAY <<< "${commands}"
  for cmd in "${CMD_ARRAY[@]}"; do
    cmd=$(echo "${cmd}" | xargs)
    base_cmd=$(printf '%s' "${cmd}" | awk '{print $1}')
    case "${base_cmd}" in
      audit)   audit="audit" ;;
      lint)    lint="lint" ;;
      test)    test="test" ;;
      refactor) refactor="${cmd}" ;;
      *)       others+=("${cmd}") ;;
    esac
  done

  local result=()
  [ -n "${audit}" ] && result+=("${audit}")
  [ -n "${lint}" ]  && result+=("${lint}")
  [ -n "${test}" ]  && result+=("${test}")
  [ -n "${refactor}" ]  && result+=("${refactor}")
  result+=("${others[@]+"${others[@]}"}")

  local IFS=','
  printf '%s\n' "${result[*]}"
}

has_lint_command() {
  local commands="$1"
  local cmd
  IFS=',' read -ra CMD_ARRAY <<< "${commands}"

  for cmd in "${CMD_ARRAY[@]}"; do
    if [ "$(echo "${cmd}" | xargs)" = "lint" ]; then
      printf '%s\n' "true"
      return 0
    fi
  done

  printf '%s\n' "false"
}

build_run_command() {
  local cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local output_file="${4:-}"
  local full_cmd

  if [[ "${cmd}" == refactor* ]]; then
    full_cmd="homeboy refactor ${component_id} ${cmd#refactor } --path ${workspace}"
  else
    full_cmd="homeboy ${cmd} ${component_id} --path ${workspace}"
  fi

  if [ -n "${output_file}" ]; then
    full_cmd="${full_cmd} --output ${output_file}"
  fi

  local scope
  scope="$(scope_flags_for "${cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}

build_autofix_command() {
  local fix_cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local output_file="${4:-}"
  local full_cmd

  if [[ "${fix_cmd}" == refactor* ]]; then
    full_cmd="homeboy refactor ${component_id} ${fix_cmd#refactor } --path ${workspace}"
  else
    full_cmd="homeboy ${fix_cmd} ${component_id} --path ${workspace}"
  fi

  if [ -n "${output_file}" ]; then
    full_cmd="${full_cmd} --output ${output_file}"
  fi

  local scope
  scope="$(scope_flags_for "${fix_cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}
