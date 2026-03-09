#!/usr/bin/env bash

set -euo pipefail

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

# Sort commands into canonical order: audit → lint → test.
# Audit is most likely to induce changes (structural fixes), lint fixes style
# on the resulting code, test validates the final state. Non-standard commands
# (release, custom extension commands) are preserved at the end in their
# original relative order.
canonicalize_commands() {
  local commands="$1"
  local audit="" lint="" test="" others=()
  local cmd

  IFS=',' read -ra CMD_ARRAY <<< "${commands}"
  for cmd in "${CMD_ARRAY[@]}"; do
    cmd=$(echo "${cmd}" | xargs)
    case "${cmd}" in
      audit)   audit="audit" ;;
      lint)    lint="lint" ;;
      test)    test="test" ;;
      *)       others+=("${cmd}") ;;
    esac
  done

  local result=()
  [ -n "${audit}" ] && result+=("${audit}")
  [ -n "${lint}" ]  && result+=("${lint}")
  [ -n "${test}" ]  && result+=("${test}")
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
  local full_cmd="homeboy ${cmd} ${component_id} --path ${workspace}"

  case "${cmd}" in
    audit)
      if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
        full_cmd="${full_cmd} --changed-since ${HOMEBOY_CHANGED_SINCE}"
      fi
      ;;
    lint)
      if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
        full_cmd="${full_cmd} --changed-since ${HOMEBOY_CHANGED_SINCE}"
      fi
      ;;
    test)
      if [ "${TEST_SCOPE:-full}" = "changed" ] && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
        full_cmd="${full_cmd} --changed-since ${HOMEBOY_CHANGED_SINCE}"
      fi
      ;;
  esac

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}

build_autofix_command() {
  local fix_cmd="$1"
  local component_id="$2"
  local workspace="$3"
  local full_cmd="homeboy ${fix_cmd} ${component_id} --path ${workspace}"

  case "${fix_cmd}" in
    lint*)
      if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
        full_cmd="${full_cmd} --changed-since ${HOMEBOY_CHANGED_SINCE}"
      fi
      ;;
    audit*)
      if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
        full_cmd="${full_cmd} --changed-since ${HOMEBOY_CHANGED_SINCE}"
      fi
      ;;
  esac

  if [ -n "${EXTRA_ARGS:-}" ]; then
    full_cmd="${full_cmd} ${EXTRA_ARGS}"
  fi

  printf '%s\n' "${full_cmd}"
}
