#!/usr/bin/env bash

set -euo pipefail

render_structured_summary() {
  local command="$1"
  local json_file="$2"
  python3 "${GITHUB_ACTION_PATH}/scripts/digest/render-command-summary.py" "${command}" "${json_file}" markdown 2>/dev/null || true
}

derive_comment_key() {
  if [ -n "${COMMENT_KEY_INPUT:-}" ]; then
    printf '%s\n' "${COMMENT_KEY_INPUT}"
  else
    printf '%s\n' "${GITHUB_WORKFLOW:-homeboy}:${COMP_ID}"
  fi
}

derive_section_key() {
  local command_count
  command_count=$(python3 - <<'PY'
import os
commands = [part.strip() for part in os.environ.get("COMMANDS", "").split(",") if part.strip()]
print(len(commands))
PY
)

  if [ -n "${COMMENT_SECTION_KEY_INPUT:-}" ]; then
    printf '%s\n' "${COMMENT_SECTION_KEY_INPUT}"
  elif [ "${command_count}" = "1" ]; then
    python3 - <<'PY'
import os
commands = [part.strip() for part in os.environ.get("COMMANDS", "").split(",") if part.strip()]
print(commands[0] if commands else os.environ.get("GITHUB_JOB", "homeboy"))
PY
  else
    printf '%s\n' "${GITHUB_JOB:-homeboy}"
  fi
}

derive_section_title() {
  if [ -n "${COMMENT_SECTION_TITLE_INPUT:-}" ]; then
    printf '%s\n' "${COMMENT_SECTION_TITLE_INPUT}"
  else
    python3 - <<'PY'
import os

provided = os.environ.get("COMMENT_SECTION_KEY_INPUT", "").strip()
commands = [part.strip() for part in os.environ.get("COMMANDS", "").split(",") if part.strip()]
if provided:
    raw = provided
elif len(commands) == 1:
    raw = commands[0]
else:
    raw = os.environ.get("GITHUB_JOB", "homeboy")

words = [word for word in raw.replace("_", "-").split("-") if word]
if not words:
    print("Homeboy")
else:
    print(" ".join(word[:1].upper() + word[1:] for word in words))
PY
  fi
}

summary_json_for_command() {
  # Resolve the structured --output JSON written by run-homeboy-commands.sh.
  # All commands use the same output stem convention.
  local command="$1"
  local stem
  stem="$(printf '%s' "${command}" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  if [ -n "${OUTPUT_DIR:-}" ] && [ -f "${OUTPUT_DIR}/${stem}.json" ]; then
    printf '%s\n' "${OUTPUT_DIR}/${stem}.json"
  else
    printf '\n'
  fi
}

command_status() {
  local command="$1"
  echo "${RESULTS}" | jq -r --arg cmd "${command}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown"
}

command_icon() {
  local status="$1"
  if [ "${status}" = "pass" ]; then
    printf '%s\n' "white_check_mark"
  else
    printf '%s\n' "x"
  fi
}

digest_covers_command() {
  local command="$1"

  if [ "${HAS_DIGEST:-false}" != "true" ]; then
    return 1
  fi

  case "${command}" in
    audit|lint|test)
      ;;
    *)
      return 1
      ;;
  esac

  local status
  status="$(command_status "${command}")"
  [ "${status}" = "fail" ]
}

append_tooling_json() {
  local file_path="$1"
  python3 -c '
import json, os, sys
tooling = {
    "homeboy_cli_version": os.environ.get("HOMEBOY_CLI_VERSION", "unknown"),
    "extension_id": os.environ.get("HOMEBOY_EXTENSION_ID", "auto"),
    "extension_source": os.environ.get("HOMEBOY_EXTENSION_SOURCE", "auto"),
    "extension_revision": os.environ.get("HOMEBOY_EXTENSION_REVISION", "unknown"),
    "action_repository": os.environ.get("HOMEBOY_ACTION_REPOSITORY", "unknown"),
    "action_ref": os.environ.get("HOMEBOY_ACTION_REF", "unknown"),
}
with open(sys.argv[1], "w") as f:
    json.dump(tooling, f)
' "${file_path}"
}

is_refactor_owning_section() {
  local section_key="${SECTION_KEY:-}"
  local section_title="${SECTION_TITLE:-}"
  local commands_csv="${COMMANDS:-}"

  case "${section_key}" in
    refactor*|auto-refactor*|*refactor*)
      return 0
      ;;
  esac

  case "${section_title}" in
    Refactor*|Auto-refactor*|*Refactor*)
      return 0
      ;;
  esac

  case "${commands_csv}" in
    *refactor*)
      return 0
      ;;
  esac

  return 1
}
