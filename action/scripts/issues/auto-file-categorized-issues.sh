#!/usr/bin/env bash
#
# File categorized GitHub issues from audit, lint, and test findings.
#
# Instead of one monolithic issue per CI run, creates one issue per finding
# category (kind). Each issue is deduplicated — if an open issue for that
# category already exists, it's updated with the new count.
#
# This is the "code factory" pattern: unfixable findings become the
# roadmap for improving the autofix system. Each category issue closes when
# its fix kind gets automated.
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
#   RESULTS               — JSON object with pass/fail per command
#   AUTOFIX_ATTEMPTED     — whether autofix was tried before filing
#   AUTOFIX_PR_CREATED    — whether an autofix PR was opened
#   BINARY_SOURCE         — how homeboy was obtained (source/release/fallback)
#
# Requires: jq, gh, python3
#

set -euo pipefail

REPO="${GITHUB_REPOSITORY}"
COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"
OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
RUN_URL="${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
AUTOFIX_ATTEMPTED="${AUTOFIX_ATTEMPTED:-false}"

HOMEBOY_CLI_VERSION="${HOMEBOY_CLI_VERSION:-unknown}"
HOMEBOY_EXTENSION_ID="${HOMEBOY_EXTENSION_ID:-auto}"
HOMEBOY_ACTION_REF="${HOMEBOY_ACTION_REF:-unknown}"
HOMEBOY_ACTION_REPOSITORY="${HOMEBOY_ACTION_REPOSITORY:-unknown}"

# Track totals across all command types
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
    groups = {}
    for f in failures:
        # Group by file
        file_key = f.get('file', 'unknown')
        groups.setdefault(file_key, []).append({
            'file': file_key,
            'description': f.get('test_name', '') + ': ' + f.get('message', ''),
            'suggestion': ''
        })
    print(json.dumps({
        'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
        'component_id': component,
        'total_findings': len(failures)
    }))
    sys.exit(0)

# Fallback: test_counts show failures — single aggregate issue
counts = data.get('test_counts', {})
failed = counts.get('failed', 0)
if failed > 0:
    total = counts.get('total', 0)
    print(json.dumps({
        'groups': {'_aggregate': []},
        'component_id': component,
        'total_findings': failed,
        'aggregate': True,
        'aggregate_label': str(failed) + ' failures out of ' + str(total) + ' tests'
    }))
    sys.exit(0)

# Baseline regression check
bc = data.get('baseline_comparison', {})
if bc and bc.get('regression', False):
    delta = abs(bc.get('failed_delta', 0))
    print(json.dumps({
        'groups': {'_aggregate': []},
        'component_id': component,
        'total_findings': max(delta, 1),
        'aggregate': True,
        'aggregate_label': str(delta) + ' new test regressions'
    }))
    sys.exit(0)

# Test passed or no failure info
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

# Tests passed — zero findings
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
    local kind_label
    kind_label=$(echo "${kind}" | tr '_' ' ')
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
    local kind_label
    kind_label=$(echo "${kind}" | tr '_' ' ')
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
# close_resolved_issues CMD_TYPE COMP_ID CURRENT_KINDS_TEXT
#
# Close issues for categories that no longer have findings.
# ─────────────────────────────────────────────────────────────────────────────

close_resolved_issues() {
  local cmd_type="$1"
  local comp_id="$2"
  local current_kinds="$3"
  local closed=0

  local existing_issues
  existing_issues=$(gh api "repos/${REPO}/issues?state=open&labels=${cmd_type}&per_page=100" \
    --jq '[.[] | {number: .number, title: .title}]' 2>/dev/null || echo "[]")

  while IFS= read -r ISSUE_LINE; do
    [ -z "${ISSUE_LINE}" ] && continue

    local issue_num issue_title
    issue_num=$(echo "${ISSUE_LINE}" | jq -r '.number')
    issue_title=$(echo "${ISSUE_LINE}" | jq -r '.title')

    # Match our component only
    if ! echo "${issue_title}" | grep -q "in ${comp_id}"; then
      continue
    fi

    # Extract kind_label: everything between "{cmd_type}: " and " in {component}"
    local kind_label kind_key
    kind_label=$(echo "${issue_title}" | sed -n "s/^${cmd_type}: \(.*\) in ${comp_id}.*/\1/p")
    [ -z "${kind_label}" ] && continue

    # Convert kind_label back to kind key (spaces → underscores)
    kind_key=$(echo "${kind_label}" | tr ' ' '_')

    # Check if this kind still has findings
    if [ -n "${current_kinds}" ] && echo "${current_kinds}" | grep -qx "${kind_key}" 2>/dev/null; then
      continue  # Still has findings — will be updated below
    fi

    # No findings for this category — close the issue
    local close_comment="All **${kind_label}** findings have been resolved. Closing automatically.

Resolved by the [code factory pipeline](${RUN_URL}). If findings reappear, a new issue will be filed."

    if ! gh api "repos/${REPO}/issues/${issue_num}/comments" \
      --method POST \
      --field body="${close_comment}" > /dev/null 2>&1; then
      echo "::warning::Failed to comment on issue #${issue_num} during close"
    fi

    if ! gh api "repos/${REPO}/issues/${issue_num}" \
      --method PATCH \
      --field state="closed" \
      --field state_reason="completed" > /dev/null 2>&1; then
      echo "::warning::Failed to close issue #${issue_num}: ${issue_title}"
    fi

    closed=$((closed + 1))
    echo "  Closed issue #${issue_num}: ${issue_title} (zero findings remaining)"
  done <<< "$(echo "${existing_issues}" | jq -c '.[]')"

  TOTAL_ISSUES_CLOSED=$((TOTAL_ISSUES_CLOSED + closed))
}

# ─────────────────────────────────────────────────────────────────────────────
# file_categorized_issues CMD_TYPE FINDINGS_JSON COMP_ID
#
# Create or update one issue per finding category.
# ─────────────────────────────────────────────────────────────────────────────

file_categorized_issues() {
  local cmd_type="$1"
  local findings_json="$2"
  local comp_id="$3"

  local total_findings is_aggregate
  total_findings=$(echo "${findings_json}" | jq -r '.total_findings')
  is_aggregate=$(echo "${findings_json}" | jq -r '.aggregate // false')

  # Extract current kinds for close-resolution
  local current_kinds=""
  if [ "${total_findings}" != "0" ] && [ "${total_findings}" != "null" ]; then
    current_kinds=$(echo "${findings_json}" | jq -r '.groups | keys[]' 2>/dev/null || true)
  fi

  # Close resolved issues for this command type
  close_resolved_issues "${cmd_type}" "${comp_id}" "${current_kinds}"

  if [ "${total_findings}" = "0" ] || [ "${total_findings}" = "null" ]; then
    echo ""
    echo "  No ${cmd_type} findings to file issues for"
    return
  fi

  local cmd_label
  cmd_label="$(echo "${cmd_type}" | sed 's/.*/\u&/')"  # Capitalize first letter

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Filing categorized ${cmd_type} issues for ${comp_id}"
  echo "  Total findings: ${total_findings}"
  echo "  Categories: $(echo "${findings_json}" | jq -r '.groups | keys | length')"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  # Fetch existing open issues for this command type
  local existing_issues
  existing_issues=$(gh api "repos/${REPO}/issues?state=open&labels=${cmd_type}&per_page=100" \
    --jq '[.[] | {number: .number, title: .title}]' 2>/dev/null || echo "[]")

  local kinds
  kinds=$(echo "${findings_json}" | jq -r '.groups | keys[]')

  while IFS= read -r KIND; do
    [ -z "${KIND}" ] && continue

    local count kind_label issue_title title_prefix existing_number
    count=$(echo "${findings_json}" | jq -r --arg k "${KIND}" '.groups[$k] | length')

    # For aggregate issues, use the aggregate_label and total count
    if [ "${is_aggregate}" = "true" ] && [ "${KIND}" = "_aggregate" ]; then
      local agg_label
      agg_label=$(echo "${findings_json}" | jq -r '.aggregate_label // "failures"')
      count="${total_findings}"
      kind_label="${agg_label}"
      issue_title="${cmd_type}: ${kind_label} in ${comp_id}"
      title_prefix="${cmd_type}: "
      # Match any issue for this cmd_type + component (aggregate issues update any existing)
      existing_number=$(echo "${existing_issues}" | jq -r --arg prefix "${cmd_type}: " --arg comp "in ${comp_id}" \
        '[.[] | select(.title | startswith($prefix) and contains($comp))] | first | .number // empty' 2>/dev/null || true)
    else
      kind_label=$(echo "${KIND}" | tr '_' ' ')
      issue_title="${cmd_type}: ${kind_label} in ${comp_id} (${count})"
      title_prefix="${cmd_type}: ${kind_label} in ${comp_id}"
      existing_number=$(echo "${existing_issues}" | jq -r --arg prefix "${title_prefix}" \
        '[.[] | select(.title | startswith($prefix))] | first | .number // empty' 2>/dev/null || true)
    fi

    # Build the findings table (only for non-aggregate issues with actual findings)
    local findings_table="" truncated_note=""
    if [ "${is_aggregate}" != "true" ] || [ "${KIND}" != "_aggregate" ]; then
      findings_table+="| File | Description | Suggestion |"$'\n'
      findings_table+="| --- | --- | --- |"$'\n'

      local category_findings
      category_findings=$(echo "${findings_json}" | jq -c --arg k "${KIND}" '.groups[$k][:50][]')

      while IFS= read -r FINDING; do
        [ -z "${FINDING}" ] && continue
        local file desc suggestion
        file=$(echo "${FINDING}" | jq -r '.file // "unknown"')
        desc=$(echo "${FINDING}" | jq -r '.description // "(no description)"' | sed 's/|/\\|/g')
        suggestion=$(echo "${FINDING}" | jq -r '.suggestion // ""' | sed 's/|/\\|/g')
        findings_table+="| \`${file}\` | ${desc} | ${suggestion} |"$'\n'
      done <<< "${category_findings}"

      if [ "${count}" -gt 50 ]; then
        truncated_note=$'\n'"*Showing 50 of ${count} findings. Run \`homeboy ${cmd_type} ${comp_id}\` locally for the full list.*"$'\n'
      fi
    fi

    local body_file
    body_file=$(mktemp)

    if [ -n "${existing_number}" ]; then
      # Update existing issue body + title
      cat > "${body_file}" <<UPDATEEOF
## ${cmd_label}: ${kind_label}

**Component:** \`${comp_id}\`
**Count:** ${count} findings
**Last run:** ${RUN_URL}
**Updated:** $(date -u +%Y-%m-%dT%H:%M:%SZ)
**Homeboy:** \`${HOMEBOY_CLI_VERSION}\` | Action: \`${HOMEBOY_ACTION_REPOSITORY}@${HOMEBOY_ACTION_REF}\`
UPDATEEOF

      if [ -n "${findings_table}" ]; then
        cat >> "${body_file}" <<TABLEEOF

### Findings

${findings_table}
${truncated_note}
TABLEEOF
      fi

      # Add autofix status section
      local autofix_section
      autofix_section=$(build_autofix_status_section "${cmd_type}" "${KIND}" "${comp_id}" "${count}" "${findings_json}")
      if [ -n "${autofix_section}" ]; then
        echo "${autofix_section}" >> "${body_file}"
      fi

      cat >> "${body_file}" <<'UPDATEFOOTEREOF'

---
*Updated automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action) on each CI run until resolved.*
UPDATEFOOTEREOF

      if ! gh api "repos/${REPO}/issues/${existing_number}" \
        --method PATCH \
        --field title="${issue_title}" \
        -F body=@"${body_file}" > /dev/null 2>&1; then
        echo "::warning::Failed to update issue #${existing_number}: ${issue_title}"
      fi

      TOTAL_ISSUES_UPDATED=$((TOTAL_ISSUES_UPDATED + 1))
      echo "  Updated issue #${existing_number}: ${issue_title}"
    else
      # Create new issue
      cat > "${body_file}" <<ISSUEEOF
## ${cmd_label}: ${kind_label}

**Component:** \`${comp_id}\`
**Count:** ${count} findings
**Run:** ${RUN_URL}
**Homeboy:** \`${HOMEBOY_CLI_VERSION}\` | Action: \`${HOMEBOY_ACTION_REPOSITORY}@${HOMEBOY_ACTION_REF}\`

### Context

This issue was filed automatically because \`homeboy ${cmd_type}\` found **${count}** \`${kind_label}\` findings that could not be auto-fixed.

Each finding in this category represents the same class of problem. Closing this issue means either:
1. The findings are resolved in the codebase, or
2. A new autofix rule handles them mechanically
ISSUEEOF

      if [ -n "${findings_table}" ]; then
        cat >> "${body_file}" <<TABLEEOF

### Findings

${findings_table}
${truncated_note}
TABLEEOF
      fi

      # Add autofix status section (per-kind fixability from audit data)
      local autofix_section
      autofix_section=$(build_autofix_status_section "${cmd_type}" "${KIND}" "${comp_id}" "${count}" "${findings_json}")
      if [ -n "${autofix_section}" ]; then
        echo "${autofix_section}" >> "${body_file}"
      elif [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
        cat >> "${body_file}" <<'AUTOFIXEOF'

### Autofix status

Autofix was attempted before filing this issue. These findings are **not yet mechanically fixable** — they need either a new fixer rule or manual resolution.
AUTOFIXEOF
      fi

      cat >> "${body_file}" <<'FOOTEREOF'

---
*Filed automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action). This issue updates on each CI run until resolved.*
FOOTEREOF

      # Try with command-type label, fall back to no labels if it doesn't exist
      gh api "repos/${REPO}/issues" \
        --method POST \
        --field title="${issue_title}" \
        -F body=@"${body_file}" \
        --field "labels[]=${cmd_type}" > /dev/null 2>&1 || \
      gh api "repos/${REPO}/issues" \
        --method POST \
        --field title="${issue_title}" \
        -F body=@"${body_file}" > /dev/null 2>&1

      TOTAL_ISSUES_CREATED=$((TOTAL_ISSUES_CREATED + 1))
      echo "  Created issue: ${issue_title}"
    fi

    rm -f "${body_file}"
  done <<< "${kinds}"
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

  # File issues for this command type
  file_categorized_issues "${CMD}" "${FINDINGS_JSON}" "${local_comp_id}"
  COMMANDS_PROCESSED=$((COMMANDS_PROCESSED + 1))
done

# ── Reconciliation: close orphaned issues for command types not in this run ──
# If a command was removed from the workflow, its issues are never updated or
# closed because the main loop only processes commands that ran this time.
# Close any open issues for command types that were NOT in this CI run.

ALL_CMD_TYPES=('audit' 'lint' 'test')
for CMD_TYPE in "${ALL_CMD_TYPES[@]}"; do
  # Skip if this command type was processed in the main loop
  if echo ",${CMD_ARRAY[*]}," | grep -q ",${CMD_TYPE},"; then
    continue
  fi
  echo "Reconciling orphaned ${CMD_TYPE} issues for ${COMP_ID}..."
  close_resolved_issues "${CMD_TYPE}" "${COMP_ID}" ""  # empty current_kinds = close all
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
