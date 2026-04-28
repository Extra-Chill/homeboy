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
  local command_count raw_key
  command_count=$(python3 - <<'PY'
import os
commands = [part.strip() for part in os.environ.get("COMMANDS", "").split(",") if part.strip()]
print(len(commands))
PY
)

  if [ -n "${COMMENT_SECTION_KEY_INPUT:-}" ]; then
    raw_key="${COMMENT_SECTION_KEY_INPUT}"
  elif [ "${command_count}" = "1" ]; then
    raw_key="$(python3 - <<'PY'
import os
commands = [part.strip() for part in os.environ.get("COMMANDS", "").split(",") if part.strip()]
print(commands[0] if commands else os.environ.get("GITHUB_JOB", "homeboy"))
PY
    )"
  else
    raw_key="${GITHUB_JOB:-homeboy}"
  fi

  SECTION_KEY_RAW="${raw_key}" python3 - <<'PY'
import os
import re

raw = os.environ.get("SECTION_KEY_RAW", "homeboy")
slug = re.sub(r"[^A-Za-z0-9._-]+", "-", raw.strip().lower())
slug = re.sub(r"-+", "-", slug).strip("-._")
print(slug or "homeboy")
PY
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
  echo "${RESULTS}" | jq -r --arg cmd "${command}" 'if .[$cmd] == "pass" or .[$cmd] == "fail" then .[$cmd] else "unknown" end' 2>/dev/null || echo "unknown"
}

command_icon() {
  local status="$1"
  case "${status}" in
    pass)
      printf '%s\n' "white_check_mark"
      ;;
    fail)
      printf '%s\n' "x"
      ;;
    *)
      printf '%s\n' "warning"
      ;;
  esac
}

command_status_note() {
  local command="$1"
  local status="$2"

  if [ "${status}" = "unknown" ]; then
    printf '%s\n' "- Could not parse a pass/fail result for ${command}; check the action logs or result artifact."
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
