#!/usr/bin/env bash
#
# File categorized GitHub issues from audit, lint, and test findings.
#
# Thin orchestrator over `homeboy runs findings reconcile`. The decision logic
# and native-output rendering live in Homeboy core with real types and tests.
# This script's only job is:
#
#   1. Locate each command's structured JSON output.
#   2. Call `homeboy runs findings reconcile --from-output ... --apply`.
#   3. Surface the reconcile plan and totals in the run log.
#
# See homeboy issue #1551 for the architectural framing. This replaces
# ~750 lines of bash + jq + `gh api` reconciliation logic with a single
# Rust call. Every consumer of homeboy now gets the same reconciliation
# behavior — cron jobs, pre-commit hooks, agent runners — for free.
#
# Supports three command types:
#   audit  — groups by finding kind (e.g. missing_method, dead_code_marker)
#   lint   — groups by category (e.g. security, i18n) or single aggregate
#   test   — groups by failure cluster category or single aggregate
#
# Env vars:
#   HOMEBOY_CI_RESULTS_DIR — directory with durable command-result/v3 files
#   COMPONENT_NAME        — component ID
#   COMMANDS              — comma-separated list of commands that were run
#   RESULTS               — JSON object with pass/fail per command
# Requires: jq, gh, homeboy
#

set -euo pipefail

COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"
OUTPUT_DIR="${HOMEBOY_CI_RESULTS_DIR:-${HOMEBOY_OUTPUT_DIR:-}}"
SETUP_RESULT_FILE="${HOMEBOY_SETUP_RESULT_FILE:-${OUTPUT_DIR:+${OUTPUT_DIR}/setup.json}}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"

# CI runners check out a single repo to GITHUB_WORKSPACE and don't have the
# component registered in homeboy's global registry. Always pass --path so
# `homeboy runs findings reconcile` discovers the component from its homeboy.json.
RECONCILE_PATH="${GITHUB_WORKSPACE:-$(pwd)}"

# Track totals across all command types — populated from reconcile output.
TOTAL_ISSUES_CREATED=0
TOTAL_ISSUES_UPDATED=0
TOTAL_ISSUES_CLOSED=0
COMMANDS_PROCESSED=0
RECONCILE_FAILURES=0

# ─────────────────────────────────────────────────────────────────────────────
# reconcile_command CMD_TYPE JSON_FILE COMP_ID
#
# Invoke `homeboy runs findings reconcile` with native command output and surface its
# plan in the run log. Core owns the command-specific rendering.
# ─────────────────────────────────────────────────────────────────────────────

reconcile_command() {
  local cmd_type="$1"
  local json_file="$2"
  local comp_id="$3"

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Reconciling ${cmd_type} issues for ${comp_id}"
  echo "  Source: ${json_file}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  local result_file
  result_file=$(mktemp)
  if ! homeboy runs findings reconcile "${comp_id}" \
    --from-output "${cmd_type}=${json_file}" \
    --run-url "${RUN_URL}" \
    --path "${RECONCILE_PATH}" \
    --apply \
    > "${result_file}" 2>&1; then
    echo "::warning::homeboy runs findings reconcile failed for ${cmd_type} — see log above"
    cat "${result_file}"
    rm -f "${result_file}"
    return 1
  fi

  # Surface the plan + per-action outcomes.
  echo "Plan:"
  jq -r '.data.plan_lines[]' "${result_file}" 2>/dev/null | sed 's/^/  /'

  # Update totals from the reconcile result.
  local filed updated closed_count
  filed=$(jq -r '[.data.result.executions[]? | select(.outcome.outcome == "filed")] | length' "${result_file}" 2>/dev/null || echo 0)
  updated=$(jq -r '[.data.result.executions[]? | select(.outcome.outcome == "updated" or .outcome.outcome == "updated_closed")] | length' "${result_file}" 2>/dev/null || echo 0)
  closed_count=$(jq -r '[.data.result.executions[]? | select(.outcome.outcome == "closed" or .outcome.outcome == "closed_duplicate")] | length' "${result_file}" 2>/dev/null || echo 0)

  TOTAL_ISSUES_CREATED=$((TOTAL_ISSUES_CREATED + filed))
  TOTAL_ISSUES_UPDATED=$((TOTAL_ISSUES_UPDATED + updated))
  TOTAL_ISSUES_CLOSED=$((TOTAL_ISSUES_CLOSED + closed_count))

  rm -f "${result_file}"
}

# ─────────────────────────────────────────────────────────────────────────────
# Main: process each command type that produced structured JSON output
# ─────────────────────────────────────────────────────────────────────────────

# Determine which commands were run from COMMANDS env or detect from JSON files
IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS:-}"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)

  # Normalize the invoked command to its base quality type. The action pipeline
  # passes review-backed commands ("review audit", "review lint", "review test")
  # as well as bare forms ("audit", "lint", "test"); both must categorize.
  # Mirrors quality_base_command() in scripts/core/lib.sh.
  case "${CMD}" in
    "review audit"*|audit|audit\ *) BASE_CMD="audit" ;;
    "review lint"*|lint|lint\ *)    BASE_CMD="lint" ;;
    "review test"*|test|test\ *)    BASE_CMD="test" ;;
    *) continue ;;
  esac

  # Resolve the structured-output file. run-homeboy-commands.sh writes to a
  # sanitized stem (command_output_stem() in lib.sh), so "review audit" lands in
  # review-audit.json, not "review audit.json". Sanitize the same way here.
  CMD_STEM="$(printf '%s' "${CMD}" | sed -E 's/[^[:alnum:]._-]+/-/g; s/^-+//; s/-+$//')"
  JSON_FILE="${OUTPUT_DIR}/${CMD_STEM}.json"

  if [ ! -f "${JSON_FILE}" ] || [ ! -s "${JSON_FILE}" ]; then
    echo "No structured result exists for ${CMD}; attempted ${JSON_FILE}"
    continue
  fi

  if ! jq empty "${JSON_FILE}" >/dev/null 2>&1; then
    echo "::warning::Structured ${CMD_STEM}.json is malformed — skipping categorized issues for ${CMD}"
    RECONCILE_FAILURES=$((RECONCILE_FAILURES + 1))
    continue
  fi

  # Resolve component ID from the JSON if available
  local_comp_id="${COMP_ID}"
  COMPONENT_FROM_JSON=$(jq -r '.data.component_id // .data.component // .component_id // .component // empty' "${JSON_FILE}")
  if [ -n "${COMPONENT_FROM_JSON}" ]; then
    local_comp_id="${COMPONENT_FROM_JSON}"
  fi

  # Reconcile this command's findings against the tracker. Pass the normalized
  # base type (audit/lint/test), not the raw "review …" form, so downstream
  # grouping and issue labels use the canonical command type.
  if reconcile_command "${BASE_CMD}" "${JSON_FILE}" "${local_comp_id}"; then
    COMMANDS_PROCESSED=$((COMMANDS_PROCESSED + 1))
  else
    RECONCILE_FAILURES=$((RECONCILE_FAILURES + 1))
  fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Categorized issues summary"
echo "  Commands processed: ${COMMANDS_PROCESSED}"
echo "  Issues created: ${TOTAL_ISSUES_CREATED}"
echo "  Issues updated: ${TOTAL_ISSUES_UPDATED}"
echo "  Issues closed:  ${TOTAL_ISSUES_CLOSED}"
echo "  Failures:       ${RECONCILE_FAILURES}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ "${RECONCILE_FAILURES}" -gt 0 ]; then
  exit 1
fi

if [ "${COMMANDS_PROCESSED}" -gt 0 ]; then
  exit 0
fi

if [ -n "${SETUP_RESULT_FILE}" ] && [ -f "${SETUP_RESULT_FILE}" ] \
  && jq -e '.schema == "homeboy/action-setup-result/v1" and (.status == "failed" or .status == "timeout")' "${SETUP_RESULT_FILE}" >/dev/null 2>&1; then
  jq -r '"Skipping finding reconciliation because " + .owner + " failed during " + .step + "; requested quality commands were not run."' "${SETUP_RESULT_FILE}"
  exit 0
fi

# ─────────────────────────────────────────────────────────────────────────────
# Empty-input handling (issue #214)
#
# COMMANDS_PROCESSED == 0 has two distinct shapes:
#
#   1. "Nothing happened here" — RESULTS is empty/`{}` AND COMMANDS is empty.
#      The wrapper invocation didn't run any categorizable command (e.g. a
#      release-only step where audit/lint/test ran in sibling jobs). There is
#      genuinely nothing to categorize, so this is a no-op success path.
#
#   2. "A command ran but failed before producing structured output" — RESULTS
#      records one of EXPECTED_COMMANDS as "fail" with no JSON to categorize.
#      The categorizer can't file findings without structured output, but it
#      must propagate the failure so the workflow doesn't ship a broken release.
#
# Anything else (commands ran, all passed, no findings) is a clean success path:
# the codebase had nothing to categorize on this run.
# ─────────────────────────────────────────────────────────────────────────────

RESULTS_JSON="${RESULTS:-}"
COMMANDS_LIST="${COMMANDS:-}"
EXPECTED_LIST="${EXPECTED_COMMANDS:-}"

# Detect commands that actually failed in this invocation's RESULTS.
FAILED_COMMANDS=""
if [ -n "${RESULTS_JSON}" ] && [ "${RESULTS_JSON}" != "{}" ]; then
  if printf '%s\n' "${RESULTS_JSON}" | jq -e 'type == "object"' >/dev/null 2>&1; then
    FAILED_COMMANDS=$(printf '%s\n' "${RESULTS_JSON}" \
      | jq -r 'to_entries | map(select(.value == "fail")) | .[].key' 2>/dev/null \
      | paste -sd ',' -)
  else
    echo "::warning::RESULTS is not a valid JSON object — treating as 'no commands ran'"
  fi
fi

if [ -n "${FAILED_COMMANDS}" ]; then
  echo "::error::Commands failed without producing structured output for categorization: ${FAILED_COMMANDS}"
  echo "Cannot file categorized issues without structured JSON. The underlying command failure should be addressed before re-running."
  exit 1
fi

# No failed commands. Empty input is a legitimate success path:
#   - release-only invocations route through RELEASE_COMMANDS, not COMMANDS
#   - clean codebases produce zero findings to categorize
#   - sibling jobs may own audit/lint/test (EXPECTED_COMMANDS lists them but
#     they didn't run in this invocation)
if [ -z "${RESULTS_JSON}" ] || [ "${RESULTS_JSON}" = "{}" ]; then
  if [ -z "${COMMANDS_LIST}" ]; then
    echo "Nothing to categorize: no categorizable commands ran in this invocation (RESULTS is empty, COMMANDS is empty)."
    if [ -n "${EXPECTED_LIST}" ]; then
      echo "Expected commands ${EXPECTED_LIST} are tracked by sibling jobs; this invocation has no findings to file."
    fi
    exit 0
  fi
fi

echo "Nothing to categorize: all categorizable commands either passed cleanly or produced no structured output to reconcile."
exit 0
