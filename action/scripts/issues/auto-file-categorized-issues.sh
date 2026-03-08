#!/usr/bin/env bash
#
# File categorized GitHub issues from audit findings.
#
# Instead of one monolithic issue per CI run, creates one issue per finding
# category (kind). Each issue is deduplicated — if an open issue for that
# category already exists, it's updated with a comment.
#
# This is the "code factory" pattern: unfixable audit findings become the
# roadmap for improving the autofix system. Each category issue closes when
# its fix kind gets automated.
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

AUDIT_JSON="${OUTPUT_DIR}/audit.json"
AUDIT_LOG="${OUTPUT_DIR}/audit.log"

# --- Step 1: Read audit findings from structured JSON ---

# Prefer the pre-extracted .json file (produced by run-homeboy-commands.sh).
# Fall back to scraping the .log file for backward compatibility.
if [ -f "${AUDIT_JSON}" ] && [ -s "${AUDIT_JSON}" ]; then
  FINDINGS_JSON=$(python3 -c "
import json, sys
payload = json.load(open(sys.argv[1]))
data = payload.get('data', {})
findings = data.get('findings', [])
summary = data.get('summary', {})
component = data.get('component_id', '')
groups = {}
for f in findings:
    kind = f.get('kind', 'unknown')
    groups.setdefault(kind, []).append(f)
print(json.dumps({
    'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
    'summary': summary,
    'component_id': component,
    'total_findings': len(findings)
}))
" "${AUDIT_JSON}" 2>/dev/null) || {
    echo "Failed to read audit.json — falling back to log scraping"
    FINDINGS_JSON=""
  }
fi

# Fall back to log scraping if JSON file is missing or failed
if [ -z "${FINDINGS_JSON:-}" ]; then
  if [ ! -f "${AUDIT_LOG}" ]; then
    echo "No audit log found at ${AUDIT_LOG} — falling back to generic issue filing"
    exit 1
  fi

  FINDINGS_JSON=$(python3 -c "
import json, sys

text = open(sys.argv[1], 'r', errors='replace').read()
lines = []
for line in text.splitlines():
    if 'Z ' in line:
        lines.append(line.rsplit('Z ', 1)[1])
    else:
        lines.append(line)
text = '\n'.join(lines)

decoder = json.JSONDecoder()
i = 0
payload = None
while i < len(text):
    if text[i] != '{':
        i += 1
        continue
    try:
        obj, end = decoder.raw_decode(text[i:])
    except json.JSONDecodeError:
        i += 1
        continue
    if isinstance(obj, dict):
        data = obj.get('data', {})
        if isinstance(data, dict) and ('findings' in data or 'summary' in data):
            payload = obj
    i += max(end, 1)

if not payload:
    sys.exit(1)

findings = payload.get('data', {}).get('findings', [])
summary = payload.get('data', {}).get('summary', {})
component = payload.get('data', {}).get('component_id', '')
groups = {}
for f in findings:
    kind = f.get('kind', 'unknown')
    groups.setdefault(kind, []).append(f)
print(json.dumps({
    'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
    'summary': summary,
    'component_id': component,
    'total_findings': len(findings)
}))
" "${AUDIT_LOG}" 2>/dev/null) || {
    echo "Failed to parse audit findings from log — falling back to generic issue filing"
    exit 1
  }
fi

TOTAL_FINDINGS=$(echo "${FINDINGS_JSON}" | jq -r '.total_findings')
COMPONENT_FROM_AUDIT=$(echo "${FINDINGS_JSON}" | jq -r '.component_id // empty')
if [ -n "${COMPONENT_FROM_AUDIT}" ]; then
  COMP_ID="${COMPONENT_FROM_AUDIT}"
fi

# --- Step 1b: Close resolved issues ---
#
# If a category has zero findings now but has an open issue, close it.
# This runs even when TOTAL_FINDINGS is 0 (all categories resolved).

EXISTING_ISSUES_FOR_CLOSE=$(gh api "repos/${REPO}/issues?state=open&labels=audit&per_page=100" \
  --jq '[.[] | {number: .number, title: .title}]' 2>/dev/null || echo "[]")

# Extract current finding kinds (empty if no findings)
CURRENT_KINDS=""
if [ "${TOTAL_FINDINGS}" != "0" ] && [ "${TOTAL_FINDINGS}" != "null" ]; then
  CURRENT_KINDS=$(echo "${FINDINGS_JSON}" | jq -r '.groups | keys[]' 2>/dev/null || true)
fi

ISSUES_CLOSED=0

# Check each existing audit issue — if its category is no longer in findings, close it
while IFS= read -r ISSUE_LINE; do
  [ -z "${ISSUE_LINE}" ] && continue

  ISSUE_NUM=$(echo "${ISSUE_LINE}" | jq -r '.number')
  ISSUE_TITLE=$(echo "${ISSUE_LINE}" | jq -r '.title')

  # Extract the kind from the issue title: "audit: {kind_label} in {component} ({count})"
  # Match our component only
  if ! echo "${ISSUE_TITLE}" | grep -q "in ${COMP_ID}"; then
    continue
  fi

  # Extract kind_label: everything between "audit: " and " in {component}"
  KIND_LABEL=$(echo "${ISSUE_TITLE}" | sed -n "s/^audit: \(.*\) in ${COMP_ID}.*/\1/p")
  [ -z "${KIND_LABEL}" ] && continue

  # Convert kind_label back to kind (spaces → underscores)
  KIND_KEY=$(echo "${KIND_LABEL}" | tr ' ' '_')

  # Check if this kind still has findings
  if echo "${CURRENT_KINDS}" | grep -qx "${KIND_KEY}" 2>/dev/null; then
    continue  # Still has findings — will be updated below
  fi

  # No findings for this category — close the issue
  CLOSE_COMMENT="All **${KIND_LABEL}** findings have been resolved. Closing automatically.

Resolved by the [code factory pipeline](${RUN_URL}). If findings reappear, a new issue will be filed."

  gh api "repos/${REPO}/issues/${ISSUE_NUM}/comments" \
    --method POST \
    --field body="${CLOSE_COMMENT}" > /dev/null 2>&1 || true

  gh api "repos/${REPO}/issues/${ISSUE_NUM}" \
    --method PATCH \
    --field state="closed" \
    --field state_reason="completed" > /dev/null 2>&1 || true

  ISSUES_CLOSED=$((ISSUES_CLOSED + 1))
  echo "  Closed issue #${ISSUE_NUM}: ${ISSUE_TITLE} (zero findings remaining)"
done <<< "$(echo "${EXISTING_ISSUES_FOR_CLOSE}" | jq -c '.[]')"

if [ "${TOTAL_FINDINGS}" = "0" ] || [ "${TOTAL_FINDINGS}" = "null" ]; then
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  No audit findings to file issues for"
  if [ "${ISSUES_CLOSED}" -gt 0 ]; then
    echo "  Issues closed: ${ISSUES_CLOSED}"
  fi
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  exit 0
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Filing categorized issues for ${COMP_ID}"
echo "  Total findings: ${TOTAL_FINDINGS}"
echo "  Categories: $(echo "${FINDINGS_JSON}" | jq -r '.groups | keys | length')"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# --- Step 2: Fetch existing open issues to deduplicate ---

EXISTING_ISSUES=$(gh api "repos/${REPO}/issues?state=open&labels=audit&per_page=100" \
  --jq '[.[] | {number: .number, title: .title}]' 2>/dev/null || echo "[]")

# --- Step 3: File one issue per finding category ---

ISSUES_CREATED=0
ISSUES_UPDATED=0

KINDS=$(echo "${FINDINGS_JSON}" | jq -r '.groups | keys[]')

while IFS= read -r KIND; do
  [ -z "${KIND}" ] && continue

  COUNT=$(echo "${FINDINGS_JSON}" | jq -r --arg k "${KIND}" '.groups[$k] | length')

  # Build human-readable kind label
  KIND_LABEL=$(echo "${KIND}" | tr '_' ' ')

  # Issue title format: audit: {kind} in {component} ({count})
  ISSUE_TITLE="audit: ${KIND_LABEL} in ${COMP_ID} (${COUNT})"

  # Check for existing open issue with same category prefix
  # Match on "audit: {kind_label} in {component}" to allow count updates
  TITLE_PREFIX="audit: ${KIND_LABEL} in ${COMP_ID}"
  EXISTING_NUMBER=$(echo "${EXISTING_ISSUES}" | jq -r --arg prefix "${TITLE_PREFIX}" \
    '[.[] | select(.title | startswith($prefix))] | first | .number // empty' 2>/dev/null || true)

  # Build the findings table for this category
  FINDINGS_TABLE=""
  FINDINGS_TABLE+="| File | Description | Suggestion |"$'\n'
  FINDINGS_TABLE+="| --- | --- | --- |"$'\n'

  # Get findings for this kind (limit to 50 per issue to avoid GitHub body limits)
  CATEGORY_FINDINGS=$(echo "${FINDINGS_JSON}" | jq -c --arg k "${KIND}" '.groups[$k][:50][]')

  while IFS= read -r FINDING; do
    [ -z "${FINDING}" ] && continue
    FILE=$(echo "${FINDING}" | jq -r '.file // "unknown"')
    DESC=$(echo "${FINDING}" | jq -r '.description // "(no description)"' | sed 's/|/\\|/g')
    SUGGESTION=$(echo "${FINDING}" | jq -r '.suggestion // ""' | sed 's/|/\\|/g')
    FINDINGS_TABLE+="| \`${FILE}\` | ${DESC} | ${SUGGESTION} |"$'\n'
  done <<< "${CATEGORY_FINDINGS}"

  TRUNCATED_NOTE=""
  if [ "${COUNT}" -gt 50 ]; then
    TRUNCATED_NOTE=$'\n'"*Showing 50 of ${COUNT} findings. Run \`homeboy audit ${COMP_ID}\` locally for the full list.*"$'\n'
  fi

  # Write the body to a temp file to avoid shell quoting issues with backticks
  BODY_FILE=$(mktemp)

  if [ -n "${EXISTING_NUMBER}" ]; then
    # Update existing issue body + title (no comments — body is the source of truth)
    cat > "${BODY_FILE}" <<UPDATEEOF
## Audit: ${KIND_LABEL}

**Component:** \`${COMP_ID}\`
**Count:** ${COUNT} findings
**Last run:** ${RUN_URL}
**Updated:** $(date -u +%Y-%m-%dT%H:%M:%SZ)
**Homeboy:** \`${HOMEBOY_CLI_VERSION}\` | Action: \`${HOMEBOY_ACTION_REPOSITORY}@${HOMEBOY_ACTION_REF}\`

### Findings

${FINDINGS_TABLE}
${TRUNCATED_NOTE}

---
*Updated automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action) on each CI run until resolved.*
UPDATEEOF

    gh api "repos/${REPO}/issues/${EXISTING_NUMBER}" \
      --method PATCH \
      --field title="${ISSUE_TITLE}" \
      -F body=@"${BODY_FILE}" > /dev/null 2>&1 || true

    ISSUES_UPDATED=$((ISSUES_UPDATED + 1))
    echo "  Updated issue #${EXISTING_NUMBER}: ${ISSUE_TITLE}"
  else
    # Create new issue
    cat > "${BODY_FILE}" <<ISSUEEOF
## Audit: ${KIND_LABEL}

**Component:** \`${COMP_ID}\`
**Count:** ${COUNT} findings
**Run:** ${RUN_URL}
**Homeboy:** \`${HOMEBOY_CLI_VERSION}\` | Action: \`${HOMEBOY_ACTION_REPOSITORY}@${HOMEBOY_ACTION_REF}\`

### Context

This issue was filed automatically because \`homeboy audit\` found **${COUNT}** \`${KIND_LABEL}\` findings that could not be auto-fixed.

Each finding in this category represents the same class of problem. Closing this issue means either:
1. The findings are resolved in the codebase, or
2. A new autofix rule handles them mechanically

### Findings

${FINDINGS_TABLE}
${TRUNCATED_NOTE}
ISSUEEOF

    if [ "${AUTOFIX_ATTEMPTED}" = "true" ]; then
      cat >> "${BODY_FILE}" <<'AUTOFIXEOF'

### Autofix status

Autofix was attempted before filing this issue. These findings are **not yet mechanically fixable** — they need either a new fixer rule or manual resolution.
AUTOFIXEOF
    fi

    cat >> "${BODY_FILE}" <<'FOOTEREOF'

---
*Filed automatically by [Homeboy Action](https://github.com/Extra-Chill/homeboy-action). This issue updates on each CI run until resolved.*
FOOTEREOF

    # Try with audit label, fall back to no labels if it doesn't exist
    gh api "repos/${REPO}/issues" \
      --method POST \
      --field title="${ISSUE_TITLE}" \
      -F body=@"${BODY_FILE}" \
      --field "labels[]=audit" > /dev/null 2>&1 || \
    gh api "repos/${REPO}/issues" \
      --method POST \
      --field title="${ISSUE_TITLE}" \
      -F body=@"${BODY_FILE}" > /dev/null 2>&1

    ISSUES_CREATED=$((ISSUES_CREATED + 1))
    echo "  Created issue: ${ISSUE_TITLE}"
  fi

  rm -f "${BODY_FILE}"
done <<< "${KINDS}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Issues created: ${ISSUES_CREATED}"
echo "  Issues updated: ${ISSUES_UPDATED}"
echo "  Issues closed:  ${ISSUES_CLOSED}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
