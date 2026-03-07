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

AUDIT_LOG="${OUTPUT_DIR}/audit.log"

# --- Step 1: Extract audit JSON from log ---

if [ ! -f "${AUDIT_LOG}" ]; then
  echo "No audit log found at ${AUDIT_LOG} — falling back to generic issue filing"
  exit 1
fi

# Extract the audit JSON payload from the log. The log contains the full
# homeboy JSON output ({"success":true,"data":{...}}) mixed with stderr.
# Use python to reliably extract it.
FINDINGS_JSON=$(AUDIT_LOG_PATH="${AUDIT_LOG}" python3 -c "
import json, os, sys

text = open(os.environ['AUDIT_LOG_PATH'], 'r', errors='replace').read()

# Strip GitHub Actions timestamp prefixes
lines = []
for line in text.splitlines():
    if 'Z ' in line:
        lines.append(line.rsplit('Z ', 1)[1])
    else:
        lines.append(line)
text = '\n'.join(lines)

# Find the audit JSON payload
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

# Group findings by kind
groups = {}
for f in findings:
    kind = f.get('kind', 'unknown')
    if kind not in groups:
        groups[kind] = []
    groups[kind].append(f)

print(json.dumps({
    'groups': {k: v for k, v in sorted(groups.items(), key=lambda x: -len(x[1]))},
    'summary': summary,
    'component_id': component,
    'total_findings': len(findings)
}))
" 2>/dev/null) || {
  echo "Failed to parse audit findings from log — falling back to generic issue filing"
  exit 1
}

TOTAL_FINDINGS=$(echo "${FINDINGS_JSON}" | jq -r '.total_findings')
COMPONENT_FROM_AUDIT=$(echo "${FINDINGS_JSON}" | jq -r '.component_id // empty')
if [ -n "${COMPONENT_FROM_AUDIT}" ]; then
  COMP_ID="${COMPONENT_FROM_AUDIT}"
fi

if [ "${TOTAL_FINDINGS}" = "0" ] || [ "${TOTAL_FINDINGS}" = "null" ]; then
  echo "No audit findings to file issues for"
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
    # Update existing issue with a comment
    cat > "${BODY_FILE}" <<COMMENTEOF
### Updated: ${COUNT} findings ($(date -u +%Y-%m-%d))

**Run:** ${RUN_URL}
**Homeboy:** \`${HOMEBOY_CLI_VERSION}\`

${FINDINGS_TABLE}
${TRUNCATED_NOTE}
COMMENTEOF

    # Update the title to reflect current count
    gh api "repos/${REPO}/issues/${EXISTING_NUMBER}" \
      --method PATCH \
      --field title="${ISSUE_TITLE}" > /dev/null 2>&1 || true

    gh api "repos/${REPO}/issues/${EXISTING_NUMBER}/comments" \
      --method POST \
      -F body=@"${BODY_FILE}" > /dev/null 2>&1

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
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
