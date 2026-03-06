#!/usr/bin/env bash

set -euo pipefail

OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
REPO="${GITHUB_REPOSITORY}"
COMP_ID="${COMPONENT_NAME:-$(basename "${GITHUB_REPOSITORY}")}"

if [ -z "${OUTPUT_DIR}" ] || [ -z "${PR_NUMBER}" ]; then
  echo "Skipping PR comment — missing output dir or PR number"
  exit 0
fi

DIGEST_FILE="${HOMEBOY_FAILURE_DIGEST_FILE:-}"

COMMENT_BODY="<!-- homeboy-action-results -->"$'\n'
COMMENT_BODY+="## Homeboy Results — \`${COMP_ID}\`"$'\n\n'

if [ "${AUTOFIX_ENABLED}" = "true" ] && [ "${AUTOFIX_COMMITTED:-}" = "true" ]; then
  COMMENT_BODY+="> :wrench: **Autofix applied** — a CI bot commit was pushed and checks were re-run"$'\n\n'
elif [ "${AUTOFIX_ENABLED}" = "true" ]; then
  COMMENT_BODY+="> :information_source: Autofix enabled, but no fixable file changes were produced"$'\n\n'
fi

if [ "${BINARY_SOURCE}" = "fallback" ]; then
  COMMENT_BODY+="> :warning: **Source build failed** — results from fallback release binary"$'\n\n'
fi

HAS_DIGEST="false"
if [ -n "${DIGEST_FILE}" ] && [ -f "${DIGEST_FILE}" ]; then
  HAS_DIGEST="true"
  COMMENT_BODY+="$(cat "${DIGEST_FILE}")"$'\n\n'
else
  COMMENT_BODY+="### Tooling versions"$'\n\n'
  COMMENT_BODY+="- Homeboy CLI: \`${HOMEBOY_CLI_VERSION:-unknown}\`"$'\n'
  COMMENT_BODY+="- Extension: \`${HOMEBOY_EXTENSION_ID:-auto}\` from \`${HOMEBOY_EXTENSION_SOURCE:-auto}\`"$'\n'
  COMMENT_BODY+="- Extension revision: \`${HOMEBOY_EXTENSION_REVISION:-unknown}\`"$'\n'
  COMMENT_BODY+="- Action: \`${HOMEBOY_ACTION_REPOSITORY:-unknown}@${HOMEBOY_ACTION_REF:-unknown}\`"$'\n\n'
fi

if [ "${TEST_SCOPE_EFFECTIVE:-}" = "full" ] && [ "${HOMEBOY_CHANGED_SINCE:-}" != "" ]; then
  COMMENT_BODY+="> :information_source: PR test scope resolved to **full** for compatibility with installed Homeboy CLI"$'\n\n'
elif [ "${TEST_SCOPE_EFFECTIVE:-}" = "changed" ]; then
  COMMENT_BODY+="> :zap: PR test scope resolved to **changed**"$'\n\n'
fi

IFS=',' read -ra CMD_ARRAY <<< "${COMMANDS}"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)
  LOG_FILE="${OUTPUT_DIR}/${CMD}.log"

  STATUS=$(echo "${RESULTS}" | jq -r --arg cmd "${CMD}" '.[$cmd] // "unknown"' 2>/dev/null || echo "unknown")

  if [ "${STATUS}" = "pass" ]; then
    ICON="white_check_mark"
  else
    ICON="x"
  fi

  SCOPE_NOTE=""
  if [ "${CMD}" = "lint" ] && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
    SCOPE_NOTE=" _(changed files only)_"
  elif [ "${CMD}" = "audit" ] && [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
    SCOPE_NOTE=" _(changed files only)_"
  fi

  COMMENT_BODY+=":${ICON}: **${CMD}**${SCOPE_NOTE}"$'\n'

  if [ -f "${LOG_FILE}" ] && [ "${HAS_DIGEST}" != "true" ]; then
    PHPCS_SUMMARY=$(grep -o "LINT SUMMARY: .*" "${LOG_FILE}" | head -1 || true)
    if [ -n "${PHPCS_SUMMARY}" ]; then
      FIXABLE=$(grep -o "Fixable: [0-9]*" "${LOG_FILE}" | head -1 || true)
      FILES_INFO=$(grep -o "Files with issues: .*" "${LOG_FILE}" | head -1 || true)
      COMMENT_BODY+="- PHPCS: ${PHPCS_SUMMARY}"$'\n'
      if [ -n "${FIXABLE}" ]; then
        COMMENT_BODY+="- ${FIXABLE} | ${FILES_INFO}"$'\n'
      fi
    fi

    TOP_VIOLATIONS=$(sed -n '/TOP VIOLATIONS:/,/^$/p' "${LOG_FILE}" | grep -E '^\s+\S' | head -5 || true)
    if [ -n "${TOP_VIOLATIONS}" ]; then
      COMMENT_BODY+=$'\n'"<details><summary>Top violations</summary>"$'\n\n'"\`\`\`"$'\n'
      COMMENT_BODY+="${TOP_VIOLATIONS}"$'\n'
      COMMENT_BODY+="\`\`\`"$'\n'"</details>"$'\n'
    fi

    PHPSTAN_SUMMARY=$(grep -o "PHPSTAN SUMMARY: .*" "${LOG_FILE}" | head -1 || true)
    if [ -n "${PHPSTAN_SUMMARY}" ]; then
      COMMENT_BODY+="- PHPStan: ${PHPSTAN_SUMMARY}"$'\n'
    fi

    BUILD_FAILED=$(grep -o "BUILD FAILED: .*" "${LOG_FILE}" | head -1 || true)
    if [ -n "${BUILD_FAILED}" ]; then
      COMMENT_BODY+="- ${BUILD_FAILED}"$'\n'
    fi

    FATAL=$(grep "PHP Fatal error:" "${LOG_FILE}" | head -1 | sed 's/.*PHP Fatal error:/Fatal:/' || true)
    if [ -n "${FATAL}" ]; then
      COMMENT_BODY+=$'\n'"<details><summary>Fatal error</summary>"$'\n\n'"\`\`\`"$'\n'
      COMMENT_BODY+="${FATAL}"$'\n'
      COMMENT_BODY+="\`\`\`"$'\n'"</details>"$'\n'
    fi

    if [ "${CMD}" = "audit" ]; then
      AUDIT_MD=$(python3 "${GITHUB_ACTION_PATH}/scripts/render-audit-summary.py" "${LOG_FILE}" 2>/dev/null || true)
      if [ -n "${AUDIT_MD}" ]; then
        COMMENT_BODY+=$'\n'"### Audit summary"$'\n'
        COMMENT_BODY+="${AUDIT_MD}"$'\n'
      fi
    fi

    CARGO_ERRORS=$(grep -c "^error\[" "${LOG_FILE}" 2>/dev/null || echo "0")
    CARGO_WARNINGS=$(grep -c "^warning\[" "${LOG_FILE}" 2>/dev/null || echo "0")
    if [[ "${CARGO_ERRORS}" =~ ^[0-9]+$ ]] && [[ "${CARGO_WARNINGS}" =~ ^[0-9]+$ ]] && { [ "${CARGO_ERRORS}" -gt 0 ] || [ "${CARGO_WARNINGS}" -gt 0 ]; }; then
      COMMENT_BODY+="- Cargo: ${CARGO_ERRORS} error(s), ${CARGO_WARNINGS} warning(s)"$'\n'
    fi

    CARGO_TEST_SUMMARY=$(grep -oE "test result: .*\. [0-9]+ passed" "${LOG_FILE}" | tail -1 || true)
    if [ -n "${CARGO_TEST_SUMMARY}" ]; then
      COMMENT_BODY+="- ${CARGO_TEST_SUMMARY}"$'\n'
    fi
  fi

  COMMENT_BODY+=$'\n'
done

COMMENT_BODY+="---"$'\n'
COMMENT_BODY+="*[Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1 — ${HOMEBOY_CLI_VERSION:-$(homeboy --version 2>/dev/null || echo 'homeboy')}*"

EXISTING_COMMENT_ID=$(gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" \
  --jq '.[] | select(.body | startswith("<!-- homeboy-action-results -->")) | .id' \
  2>/dev/null | head -1 || true)

if [ -n "${EXISTING_COMMENT_ID}" ]; then
  echo "Updating existing comment ${EXISTING_COMMENT_ID}..."
  if ! gh api "repos/${REPO}/issues/comments/${EXISTING_COMMENT_ID}" \
    --method PATCH \
    --field body="${COMMENT_BODY}" > /dev/null 2>&1; then
    echo "::warning::Could not update PR comment (likely restricted token for fork PR). Skipping comment publish."
    exit 0
  fi
else
  echo "Creating new comment..."
  if ! gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" \
    --method POST \
    --field body="${COMMENT_BODY}" > /dev/null 2>&1; then
    echo "::warning::Could not create PR comment (likely restricted token for fork PR). Skipping comment publish."
    exit 0
  fi
fi

echo "PR comment posted successfully"
