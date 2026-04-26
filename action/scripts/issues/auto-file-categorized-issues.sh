#!/usr/bin/env bash
#
# File categorized GitHub issues from audit, lint, and test findings.
#
# Thin orchestrator over `homeboy issues reconcile` (homeboy v0.99+). The
# decision logic — file new / update / close / dedupe / suppress — lives in
# Rust with real types and tests. This script's only job is:
#
#   1. Normalize each command's structured JSON into the canonical
#      findings shape (groups by category + count).
#   2. Render markdown bodies for each group using the action's templates
#      (autofix status, finding tables, footers).
#   3. Pipe the canonical JSON to `homeboy issues reconcile --apply --json`
#      and surface its plan in the run log.
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
#   HOMEBOY_OUTPUT_DIR    — directory with command log files
#   COMPONENT_NAME        — component ID
#   COMMANDS              — comma-separated list of commands that were run
#   EXPECTED_COMMANDS     — optional; comma-separated list of command types
#                           expected to run across the full workflow. Used to
#                           scope the orphan-reconciliation step so workflows
#                           which split audit/lint/test across separate
#                           invocations do not close each other's issues.
#                           Defaults to COMMANDS when empty.
#   RESULTS               — JSON object with pass/fail per command
#   AUTOFIX_ATTEMPTED     — whether autofix was tried before filing
#   AUTOFIX_PR_CREATED    — whether an autofix PR was opened
#   BINARY_SOURCE         — how homeboy was obtained (source/release/fallback)
#
# Requires: jq, gh, python3, homeboy v0.99+
#

set -euo pipefail

REPO="${GITHUB_REPOSITORY}"
COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"
OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
AUTOFIX_ATTEMPTED="${AUTOFIX_ATTEMPTED:-false}"

# CI runners check out a single repo to GITHUB_WORKSPACE and don't have the
# component registered in homeboy's global registry. Always pass --path so
# `homeboy issues reconcile` discovers the component from its homeboy.json.
RECONCILE_PATH="${GITHUB_WORKSPACE:-$(pwd)}"

HOMEBOY_CLI_VERSION="${HOMEBOY_CLI_VERSION:-unknown}"
HOMEBOY_EXTENSION_ID="${HOMEBOY_EXTENSION_ID:-auto}"
HOMEBOY_ACTION_REF="${HOMEBOY_ACTION_REF:-unknown}"
HOMEBOY_ACTION_REPOSITORY="${HOMEBOY_ACTION_REPOSITORY:-unknown}"

# Track totals across all command types — populated from reconcile output.
TOTAL_ISSUES_CREATED=0
TOTAL_ISSUES_UPDATED=0
TOTAL_ISSUES_CLOSED=0
COMMANDS_PROCESSED=0

# ─────────────────────────────────────────────────────────────────────────────
# Normalizers: each command type produces the same intermediate JSON format
#
#   {
#     "groups": { "kind_key": [ {file, description, suggestion}, ... ], ... },
#     "total_findings": N,
#     "component_id": "comp"
#   }
#
# For aggregate-only results (no per-finding detail), groups has one key:
#   { "groups": { "_aggregate": [] }, "total_findings": N, "aggregate": true }
# ─────────────────────────────────────────────────────────────────────────────

normalize_audit_json() {
  local json_file="$1"
  jq '{
    groups: (if .data.findings then (.data.findings | group_by(.kind) | map({key: .[0].kind, value: [.[] | {file: (.file // "unknown"), description: (.description // ""), suggestion: (.suggestion // "")}]}) | from_entries | to_entries | sort_by(-(.value | length)) | from_entries) else {} end),
    component_id: (.data.component_id // ""),
    total_findings: (.data.findings | length),
    fixability: (if .data.fixability and .data.fixability.by_kind then (.data.fixability.by_kind | map_values({total: (.total // 0), safe: (.automated // 0), plan_only: (.manual_only // 0)})) else {} end)
  }' "${json_file}" 2>/dev/null
}

normalize_lint_json() {
  local json_file="$1"
  python3 -c "
import json, sys
payload = json.load(open(sys.argv[1]))
data = payload.get('data', {})
status = data.get('status', 'unknown')

# Primary: use lint_findings grouped by category (available with baseline)
lint_findings = data.get('lint_findings', [])
if lint_findings:
    groups = {}
    for f in lint_findings:
        cat = f.get('category', 'uncategorized')
        groups.setdefault(cat, []).append({
            'file': f.get('id', '').split('::')[0] if '::' in f.get('id', '') else 'unknown',
            'description': f.get('message', ''),
            'suggestion': ''
        })
    print(json.dumps({
        'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
        'component_id': data.get('component', ''),
        'total_findings': len(lint_findings)
    }))
    sys.exit(0)

# Fallback: baseline_comparison has new_items (items above baseline)
bc = data.get('baseline_comparison', {})
if bc:
    new_items = bc.get('new_items', [])
    if new_items:
        groups = {}
        for item in new_items:
            label = item.get('context_label', 'lint:unknown')
            # context_label format: 'lint:category' — extract category
            cat = label.split(':', 1)[-1] if ':' in label else label
            groups.setdefault(cat, []).append({
                'file': 'unknown',
                'description': item.get('description', ''),
                'suggestion': ''
            })
        print(json.dumps({
            'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
            'component_id': data.get('component', ''),
            'total_findings': len(new_items)
        }))
        sys.exit(0)
    delta = bc.get('delta', 0)
    if delta > 0:
        # Baseline regression but no itemized findings — aggregate
        print(json.dumps({
            'groups': {'_aggregate': []},
            'component_id': data.get('component', ''),
            'total_findings': delta,
            'aggregate': True,
            'aggregate_label': str(delta) + ' new findings above baseline'
        }))
        sys.exit(0)

# Last resort: lint failed but no structured findings — single aggregate issue
if status == 'failed':
    exit_code = data.get('exit_code', 1)
    print(json.dumps({
        'groups': {'_aggregate': []},
        'component_id': data.get('component', ''),
        'total_findings': exit_code,
        'aggregate': True,
        'aggregate_label': 'lint failure (exit ' + str(exit_code) + ')'
    }))
    sys.exit(0)

# Lint passed — report zero findings (triggers auto-close of resolved issues)
print(json.dumps({
    'groups': {},
    'component_id': data.get('component', ''),
    'total_findings': 0
}))
" "${json_file}" 2>/dev/null
}

normalize_test_json() {
  local json_file="$1"
  python3 -c "
import json, sys
payload = json.load(open(sys.argv[1]))
data = payload.get('data', {})
status = data.get('status', 'unknown')
component = data.get('component', '')

# Primary: use analysis clusters (detailed failure grouping)
analysis = data.get('analysis', {})
if analysis and analysis.get('clusters'):
    clusters = analysis['clusters']
    groups = {}
    for c in clusters:
        cat = c.get('category', 'unknown')
        count = c.get('count', 1)
        for test in c.get('example_tests', [])[:count]:
            groups.setdefault(cat, []).append({
                'file': ', '.join(c.get('affected_files', ['unknown'])[:3]),
                'description': c.get('pattern', ''),
                'suggestion': c.get('suggested_fix', '')
            })
        # If example_tests is empty or less than count, pad with the cluster info
        existing = len(groups.get(cat, []))
        for _ in range(count - existing):
            groups.setdefault(cat, []).append({
                'file': ', '.join(c.get('affected_files', ['unknown'])[:3]),
                'description': c.get('pattern', ''),
                'suggestion': c.get('suggested_fix', '')
            })
    total = analysis.get('total_failures', sum(len(v) for v in groups.values()))
    print(json.dumps({
        'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
        'component_id': component,
        'total_findings': total
    }))
    sys.exit(0)

# Secondary: use summary.failures (from --json-summary)
summary = data.get('summary', {})
if summary and summary.get('failures'):
    failures = summary['failures']
    print(json.dumps({
        'groups': {'_aggregate': []},
        'component_id': component,
        'total_findings': failures,
        'aggregate': True,
        'aggregate_label': str(failures) + ' test failures'
    }))
    sys.exit(0)

# Last resort: test failed but no structured failures — single aggregate issue
if status == 'failed':
    exit_code = data.get('exit_code', 1)
    print(json.dumps({
        'groups': {'_aggregate': []},
        'component_id': component,
        'total_findings': exit_code,
        'aggregate': True,
        'aggregate_label': 'test failure (exit ' + str(exit_code) + ')'
    }))
    sys.exit(0)

# Test passed — report zero findings (triggers auto-close of resolved issues)
print(json.dumps({
    'groups': {},
    'component_id': component,
    'total_findings': 0
}))
" "${json_file}" 2>/dev/null
}

# ─────────────────────────────────────────────────────────────────────────────
# build_autofix_status_section CMD_TYPE KIND COMP_ID COUNT FINDINGS_JSON
#
# Build the autofix status markdown section for an issue body.
# Returns the section on stdout (empty string if no fixability data).
# ─────────────────────────────────────────────────────────────────────────────

build_autofix_status_section() {
  local cmd_type="$1"
  local kind="$2"
  local comp_id="$3"
  local count="$4"
  local findings_json="$5"

  # Only audit issues have per-kind fixability data
  if [ "${cmd_type}" != "audit" ]; then
    return
  fi

  # Extract fixability for this kind
  local fix_data
  fix_data=$(echo "${findings_json}" | jq -c --arg k "${kind}" '.fixability[$k] // empty' 2>/dev/null || true)

  if [ -z "${fix_data}" ] || [ "${fix_data}" = "null" ]; then
    # No fixer available for this category
    cat <<NOFIXEOF

### Autofix status

❌ No fixer available for \`${kind}\`
NOFIXEOF
    return
  fi

  local fix_total fix_safe fix_plan_only
  fix_total=$(echo "${fix_data}" | jq -r '.total // 0')
  fix_safe=$(echo "${fix_data}" | jq -r '.safe // 0')
  fix_plan_only=$(echo "${fix_data}" | jq -r '.plan_only // 0')

  if [ "${fix_total}" -eq 0 ]; then
    cat <<NOFIXEOF

### Autofix status

❌ No fixer available for \`${kind}\`
NOFIXEOF
    return
  fi

  local status_icon status_text
  if [ "${fix_total}" -ge "${count}" ]; then
    status_icon="✅"
    status_text="${fix_total}/${count} findings auto-fixable"
  elif [ "${fix_total}" -gt 0 ]; then
    local skipped=$((count - fix_total))
    status_icon="⚠️"
    status_text="${fix_total}/${count} findings auto-fixable (${skipped} require manual fix)"
  fi

  local tier_note=""
  if [ "${fix_safe}" -gt 0 ] && [ "${fix_plan_only}" -gt 0 ]; then
    tier_note=$'\n'"- **${fix_safe}** safe (auto-applied) · **${fix_plan_only}** plan-only (needs review)"
  elif [ "${fix_plan_only}" -gt 0 ]; then
    tier_note=$'\n'"- All fixes are **plan-only** (preview, needs human review)"
  fi

  cat <<FIXEOF

### Autofix status

${status_icon} ${status_text}${tier_note}
Run: \`homeboy refactor ${comp_id} --from audit --write --only ${kind}\`
FIXEOF
}

# ─────────────────────────────────────────────────────────────────────────────
# render_group_body CMD_TYPE KIND COUNT FINDINGS_JSON IS_AGGREGATE AGGREGATE_LABEL
#
# Render a single group's full markdown body — the same template the bash
# was writing inline before. Output goes to stdout for capture by the
# canonical-payload builder.
# ─────────────────────────────────────────────────────────────────────────────

render_group_body() {
  local cmd_type="$1"
  local kind="$2"
  local count="$3"
  local findings_json="$4"
  local is_aggregate="$5"
  local aggregate_label="$6"

  local cmd_label kind_label
  cmd_label="$(echo "${cmd_type}" | sed 's/.*/\u&/')"  # Capitalize first letter
  if [ "${is_aggregate}" = "true" ] && [ "${kind}" = "_aggregate" ]; then
    kind_label="${aggregate_label}"
  else
    kind_label=$(echo "${kind}" | tr '_' ' ')
  fi

  cat <<HEADER
## ${cmd_label}: ${kind_label}

**Component:** \`${COMP_ID}\`
**Count:** ${count} findings
**Last run:** ${RUN_URL}
**Updated:** $(date -u +%Y-%m-%dT%H:%M:%SZ)
**Homeboy:** \`${HOMEBOY_CLI_VERSION}\` | Action: \`${HOMEBOY_ACTION_REPOSITORY}@${HOMEBOY_ACTION_REF}\`
HEADER

  # Findings table (skip for aggregate groups)
  if [ "${is_aggregate}" != "true" ] || [ "${kind}" != "_aggregate" ]; then
    local category_findings
    category_findings=$(echo "${findings_json}" | jq -c --arg k "${kind}" '.groups[$k][:50][]')

    if [ -n "${category_findings}" ]; then
      printf '\n### Findings\n\n| File | Description | Suggestion |\n| --- | --- | --- |\n'
      while IFS= read -r FINDING; do
        [ -z "${FINDING}" ] && continue
        local file desc suggestion
        file=$(echo "${FINDING}" | jq -r '.file // "unknown"')
        desc=$(echo "${FINDING}" | jq -r '.description // "(no description)"' | sed 's/|/\\|/g')
        suggestion=$(echo "${FINDING}" | jq -r '.suggestion // ""' | sed 's/|/\\|/g')
        printf '| `%s` | %s | %s |\n' "${file}" "${desc}" "${suggestion}"
      done <<< "${category_findings}"

      if [ "${count}" -gt 50 ]; then
        printf '\n*Showing 50 of %s findings. Run `homeboy %s %s` locally for the full list.*\n' \
          "${count}" "${cmd_type}" "${COMP_ID}"
      fi
    fi
  fi

  # Autofix status section
  local autofix_section
  autofix_section=$(build_autofix_status_section "${cmd_type}" "${kind}" "${COMP_ID}" "${count}" "${findings_json}")
  if [ -n "${autofix_section}" ]; then
    echo "${autofix_section}"
  elif [ "${AUTOFIX_ATTEMPTED}" = "true" ] && [ "${cmd_type}" != "audit" ]; then
    cat <<'AUTOFIXEOF'

### Autofix status

Autofix was attempted before filing this issue. These findings are **not yet mechanically fixable** — they need either a new fixer rule or manual resolution.
AUTOFIXEOF
  fi

  cat <<'FOOTEREOF'

---
*Maintained automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action) on each CI run until resolved.*
FOOTEREOF
}

# ─────────────────────────────────────────────────────────────────────────────
# build_reconcile_input CMD_TYPE FINDINGS_JSON COMP_ID
#
# Translate the action's intermediate findings JSON into the input shape
# `homeboy issues reconcile` expects:
#
#   {
#     "command": "audit",
#     "groups": {
#       "<category>": { "count": N, "label": "...", "body": "<rendered md>" },
#       ...
#     }
#   }
#
# Per-group `body` is rendered from the action's templates, so the reconciler
# stays format-agnostic. Categories with `count: 0` (which would never come
# from a real findings stream — that's reconcile's "no findings remaining"
# row) are not emitted here; close-on-resolved is driven by the absence of
# a category from `groups` versus its presence in the existing tracker.
# ─────────────────────────────────────────────────────────────────────────────

build_reconcile_input() {
  local cmd_type="$1"
  local findings_json="$2"
  local _comp_id="$3"
  local out_file="$4"

  local total_findings is_aggregate aggregate_label
  total_findings=$(echo "${findings_json}" | jq -r '.total_findings')
  is_aggregate=$(echo "${findings_json}" | jq -r '.aggregate // false')
  aggregate_label=$(echo "${findings_json}" | jq -r '.aggregate_label // "failures"')

  local kinds
  kinds=$(echo "${findings_json}" | jq -r '.groups | keys[]')

  # Build the groups object incrementally with jq, injecting each rendered
  # body. Start with an empty object.
  local payload_file
  payload_file=$(mktemp)
  jq -n --arg cmd "${cmd_type}" '{command: $cmd, groups: {}}' > "${payload_file}"

  while IFS= read -r KIND; do
    [ -z "${KIND}" ] && continue

    local count kind_label body
    count=$(echo "${findings_json}" | jq -r --arg k "${KIND}" '.groups[$k] | length')
    if [ "${is_aggregate}" = "true" ] && [ "${KIND}" = "_aggregate" ]; then
      count="${total_findings}"
      kind_label="${aggregate_label}"
    else
      kind_label=$(echo "${KIND}" | tr '_' ' ')
    fi

    body=$(render_group_body "${cmd_type}" "${KIND}" "${count}" "${findings_json}" \
      "${is_aggregate}" "${aggregate_label}")

    # Merge this group into the payload.
    local next_file
    next_file=$(mktemp)
    jq --arg k "${KIND}" \
       --argjson c "${count}" \
       --arg label "${kind_label}" \
       --arg body "${body}" \
       '.groups[$k] = {count: $c, label: $label, body: $body}' \
       "${payload_file}" > "${next_file}"
    mv "${next_file}" "${payload_file}"
  done <<< "${kinds}"

  mv "${payload_file}" "${out_file}"
}

# ─────────────────────────────────────────────────────────────────────────────
# reconcile_command CMD_TYPE FINDINGS_JSON COMP_ID
#
# Build the canonical findings payload for `homeboy issues reconcile`,
# invoke it, and surface its plan in the run log.
# ─────────────────────────────────────────────────────────────────────────────

reconcile_command() {
  local cmd_type="$1"
  local findings_json="$2"
  local comp_id="$3"

  local total_findings
  total_findings=$(echo "${findings_json}" | jq -r '.total_findings')

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Reconciling ${cmd_type} issues for ${comp_id}"
  echo "  Total findings: ${total_findings}"
  echo "  Categories: $(echo "${findings_json}" | jq -r '.groups | keys | length')"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  local input_file
  input_file=$(mktemp)
  build_reconcile_input "${cmd_type}" "${findings_json}" "${comp_id}" "${input_file}"

  # Sanity check: the input file must be valid JSON or homeboy will fail
  # noisily — log the problem and bail with a useful message.
  if ! jq empty "${input_file}" >/dev/null 2>&1; then
    echo "::warning::Failed to build reconcile input for ${cmd_type} (malformed JSON)"
    rm -f "${input_file}"
    return 1
  fi

  local result_file
  result_file=$(mktemp)
  if ! homeboy issues reconcile "${comp_id}" \
    --findings "${input_file}" \
    --path "${RECONCILE_PATH}" \
    --apply \
    --suppress-from-config \
    > "${result_file}" 2>&1; then
    echo "::warning::homeboy issues reconcile failed for ${cmd_type} — see log above"
    cat "${result_file}"
    rm -f "${input_file}" "${result_file}"
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

  rm -f "${input_file}" "${result_file}"
}

# ─────────────────────────────────────────────────────────────────────────────
# Main: process each command type that produced structured JSON output
# ─────────────────────────────────────────────────────────────────────────────

# Determine which commands were run from COMMANDS env or detect from JSON files
IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS:-}"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)

  # Only process command types we know how to normalize
  case "${CMD}" in
    audit|lint|test) ;;
    *) continue ;;
  esac

  JSON_FILE="${OUTPUT_DIR}/${CMD}.json"

  if [ ! -f "${JSON_FILE}" ] || [ ! -s "${JSON_FILE}" ]; then
    echo "No structured ${CMD}.json found — skipping categorized issues for ${CMD}"
    continue
  fi

  # Normalize the JSON into the common intermediate format
  FINDINGS_JSON=""
  case "${CMD}" in
    audit) FINDINGS_JSON=$(normalize_audit_json "${JSON_FILE}") ;;
    lint)  FINDINGS_JSON=$(normalize_lint_json "${JSON_FILE}") ;;
    test)  FINDINGS_JSON=$(normalize_test_json "${JSON_FILE}") ;;
  esac

  if [ -z "${FINDINGS_JSON}" ]; then
    echo "Failed to normalize ${CMD}.json — skipping categorized issues for ${CMD}"
    continue
  fi

  # Resolve component ID from the JSON if available
  local_comp_id="${COMP_ID}"
  COMPONENT_FROM_JSON=$(echo "${FINDINGS_JSON}" | jq -r '.component_id // empty')
  if [ -n "${COMPONENT_FROM_JSON}" ]; then
    local_comp_id="${COMPONENT_FROM_JSON}"
  fi

  # Reconcile this command's findings against the tracker
  if reconcile_command "${CMD}" "${FINDINGS_JSON}" "${local_comp_id}"; then
    COMMANDS_PROCESSED=$((COMMANDS_PROCESSED + 1))
  fi
done

# ── Reconciliation: close orphaned issues for command types not in this run ──
# If a command was removed from the workflow, its issues are never updated or
# closed because the main loop only processes commands that ran this time.
# Close any open issues for command types that were NOT in this CI run.
#
# Scope: workflows that split audit/lint/test across separate invocations
# (e.g. one step per command + a final autofix step) must pass the full set
# as `expected-commands` so each invocation only reconciles command types
# that no invocation in the workflow will handle. Without it, an invocation
# running only `audit` would treat every open lint/test issue as orphaned
# and close it — even though a sibling invocation will file lint/test
# issues seconds later.
#
# Default (EXPECTED_COMMANDS empty): fall back to COMMANDS so single-command
# invocations still reconcile siblings the bot once owned but no longer runs.
#
# Implementation: for each orphan command type, invoke `homeboy issues
# reconcile` with an empty groups object. Since no findings exist, every
# open issue for that command type drops to row 3 of the contract (close
# with reason=completed). closed-not_planned issues are left alone.

if [ -n "${EXPECTED_COMMANDS:-}" ]; then
  IFS=',' read -ra EXPECTED_CMD_ARRAY <<< "${EXPECTED_COMMANDS}"
  for i in "${!EXPECTED_CMD_ARRAY[@]}"; do
    EXPECTED_CMD_ARRAY[$i]=$(echo "${EXPECTED_CMD_ARRAY[$i]}" | xargs)
  done
else
  EXPECTED_CMD_ARRAY=("${CMD_ARRAY[@]}")
fi

ALL_CMD_TYPES=('audit' 'lint' 'test')
for CMD_TYPE in "${ALL_CMD_TYPES[@]}"; do
  EXPECTED_JOINED=$(IFS=','; echo "${EXPECTED_CMD_ARRAY[*]}")
  if echo ",${EXPECTED_JOINED}," | grep -q ",${CMD_TYPE},"; then
    continue
  fi
  echo ""
  echo "Reconciling orphaned ${CMD_TYPE} issues for ${COMP_ID}..."

  # Empty groups payload triggers row-3 close-on-zero-findings for every
  # open issue for this command type. The reconciler is a single source of
  # truth for "what does close-on-resolved mean" — we just hand it nothing.
  orphan_input=$(mktemp)
  jq -n --arg cmd "${CMD_TYPE}" '{command: $cmd, groups: {}}' > "${orphan_input}"

  orphan_result=$(mktemp)
  if homeboy issues reconcile "${COMP_ID}" \
    --findings "${orphan_input}" \
    --path "${RECONCILE_PATH}" \
    --apply \
    --suppress-from-config \
    > "${orphan_result}" 2>&1; then
    jq -r '.data.plan_lines[]' "${orphan_result}" 2>/dev/null | sed 's/^/  /' || true
    closed_count=$(jq -r '[.data.result.executions[]? | select(.outcome.outcome == "closed" or .outcome.outcome == "closed_duplicate")] | length' "${orphan_result}" 2>/dev/null || echo 0)
    TOTAL_ISSUES_CLOSED=$((TOTAL_ISSUES_CLOSED + closed_count))
  else
    echo "::warning::Failed to reconcile orphaned ${CMD_TYPE} issues"
    cat "${orphan_result}"
  fi

  rm -f "${orphan_input}" "${orphan_result}"
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Categorized issues summary"
echo "  Commands processed: ${COMMANDS_PROCESSED}"
echo "  Issues created: ${TOTAL_ISSUES_CREATED}"
echo "  Issues updated: ${TOTAL_ISSUES_UPDATED}"
echo "  Issues closed:  ${TOTAL_ISSUES_CLOSED}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Exit 1 if no commands were successfully processed — lets generic fallback handle it
if [ "${COMMANDS_PROCESSED}" -eq 0 ]; then
  echo "No commands produced valid structured output for categorized issues"
  exit 1
fi
