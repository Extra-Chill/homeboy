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
  local finding_types="${3:-}"

  local subject="${AUTOFIX_COMMIT_PREFIX}"
  if [ -n "${fix_types}" ]; then
    subject="${subject} — ${fix_types}"
  fi
  if [ -n "${finding_types}" ]; then
    subject="${subject} [${finding_types}]"
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

resolve_pr_target_repo() {
  if [ -n "${PR_HEAD_REPO:-}" ]; then
    printf '%s\n' "${PR_HEAD_REPO}"
  else
    printf '%s\n' "${GITHUB_REPOSITORY}"
  fi
}

resolve_pr_target_branch() {
  if [ -n "${GITHUB_HEAD_REF:-}" ]; then
    printf '%s\n' "${GITHUB_HEAD_REF}"
  elif [ -n "${GITHUB_REF_NAME:-}" ]; then
    printf '%s\n' "${GITHUB_REF_NAME}"
  else
    git rev-parse --abbrev-ref HEAD 2>/dev/null || true
  fi
}

build_github_remote_url() {
  local repo="$1"
  local token="${2:-}"

  if [ -n "${token}" ]; then
    printf 'https://x-access-token:%s@github.com/%s.git\n' "${token}" "${repo}"
  else
    printf 'https://github.com/%s.git\n' "${repo}"
  fi
}

resolve_push_target() {
  local repo="$1"
  local token="${2:-}"

  if [ -n "${token}" ]; then
    build_github_remote_url "${repo}" "${token}"
  elif [ "${repo}" = "${GITHUB_REPOSITORY:-}" ]; then
    printf 'origin\n'
  else
    build_github_remote_url "${repo}"
  fi
}

# Sort commands into canonical order: audit → lint → test → refactor.
# Audit/lint/test are the core quality gates; real refactor commands run after
# them when explicitly requested.
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
  [ -n "${refactor}" ] && result+=("${refactor}")
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
  local global_flags=""

  # --output is a global flag and must appear before the subcommand
  # (clap global args don't propagate when placed after positional args)
  if [ -n "${output_file}" ]; then
    global_flags="--output ${output_file} "
  fi

  if [[ "${cmd}" == refactor* ]]; then
    full_cmd="homeboy ${global_flags}refactor ${component_id} ${cmd#refactor } --path ${workspace}"
  else
    full_cmd="homeboy ${global_flags}${cmd} ${component_id} --path ${workspace}"
  fi

  local scope
  scope="$(scope_flags_for "${cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}

command_output_stem() {
  local cmd="$1"
  local stem
  stem="$(printf '%s' "${cmd}" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  stem="${stem#-}"
  stem="${stem%-}"
  if [ -z "${stem}" ]; then
    stem="homeboy-output"
  fi
  printf '%s\n' "${stem}"
}

# Validate that staged autofix changes compile.
#
# Ideally this would use the component's extension validate script (scripts.validate
# in the extension manifest), but there's no `homeboy validate` CLI command yet.
# As a pragmatic fallback, detect the project type from workspace files and run
# the appropriate compiler check. This is a safety net — the proper fix is to wire
# validate_write into the refactor pipeline's chunk verifier.
#
# Returns 0 if valid (or no validator found), 1 if compilation fails.
validate_autofix_compilation() {
  local workspace="${1:-.}"
  local component_id="${2:-}"

  # Try homeboy's own build command first (uses extension configuration)
  if [ -n "${component_id}" ] && command -v homeboy &> /dev/null; then
    echo "Validating autofix changes via homeboy build..."
    set +e
    homeboy build "${component_id}" --path "${workspace}" 2>&1 | tail -30
    local hb_exit=${PIPESTATUS[0]}
    set -e

    if [ "${hb_exit}" -ne 0 ]; then
      echo "::error::Autofix changes do not compile (homeboy build exit ${hb_exit})"
      return 1
    fi
    echo "Autofix changes compile successfully (homeboy build)"
    return 0
  fi

  # Fallback: detect project type from workspace files
  local validate_cmd=""
  if [ -f "${workspace}/Cargo.toml" ]; then
    validate_cmd="cargo check --manifest-path ${workspace}/Cargo.toml"
  elif [ -f "${workspace}/tsconfig.json" ]; then
    validate_cmd="npx tsc --noEmit --project ${workspace}/tsconfig.json"
  elif [ -f "${workspace}/composer.json" ]; then
    # PHP: check syntax on changed files
    validate_cmd="git diff --cached --name-only --diff-filter=ACMR -- '*.php' | xargs -r -n1 php -l"
  elif [ -f "${workspace}/go.mod" ]; then
    validate_cmd="go vet ./..."
  fi

  if [ -z "${validate_cmd}" ]; then
    echo "No compilation validator detected — skipping"
    return 0
  fi

  echo "Validating autofix changes compile: ${validate_cmd}"
  set +e
  eval "${validate_cmd}" 2>&1 | tail -30
  local exit_code=${PIPESTATUS[0]}
  set -e

  if [ "${exit_code}" -ne 0 ]; then
    echo "::error::Autofix changes do not compile (exit ${exit_code})"
    return 1
  fi

  echo "Autofix changes compile successfully"
  return 0
}

build_autofix_command() {
  local fix_cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local output_file="${4:-}"
  local full_cmd
  local global_flags=""

  # --output is a global flag and must appear before the subcommand
  if [ -n "${output_file}" ]; then
    global_flags="--output ${output_file} "
  fi

  if [[ "${fix_cmd}" == refactor* ]]; then
    full_cmd="homeboy ${global_flags}refactor ${component_id} ${fix_cmd#refactor } --path ${workspace}"
  else
    full_cmd="homeboy ${global_flags}${fix_cmd} ${component_id} --path ${workspace}"
  fi

  local scope
  scope="$(scope_flags_for "${fix_cmd}")"
  [ -n "${scope}" ] && full_cmd="${full_cmd} ${scope}"

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}
