#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" != *"${needle}"* ]]; then
    printf 'FAIL: %s\nmissing: %s\nbody:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" == *"${needle}"* ]]; then
    printf 'FAIL: %s\nunexpected: %s\nbody:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

FAKE_BIN="${TMP_DIR}/bin"
mkdir -p "${FAKE_BIN}"

cat > "${FAKE_BIN}/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

body_file=""
is_find="false"
is_edit="false"
is_create="false"
for arg in "$@"; do
  [ "${arg}" = "find" ] && is_find="true"
  [ "${arg}" = "edit" ] && is_edit="true"
  [ "${arg}" = "create" ] && is_create="true"
done
while [ "$#" -gt 0 ]; do
  case "$1" in
    --body-file)
      body_file="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [ "${is_find}" = "true" ]; then
  printf '{"data":{"items":[]}}\n'
elif [ "${is_create}" = "true" ] || [ "${is_edit}" = "true" ]; then
  cp "${body_file}" "${BODY_CAPTURE}"
  printf '{"data":{"url":"https://github.com/Extra-Chill/homeboy-action/pull/99"}}\n'
fi
SH
chmod +x "${FAKE_BIN}/homeboy"

cat > "${FAKE_BIN}/gh" <<'SH'
#!/usr/bin/env bash
printf 'main\n'
SH
chmod +x "${FAKE_BIN}/gh"

export PATH="${FAKE_BIN}:${PATH}"
export GITHUB_ACTION_PATH="${ROOT}"
export GITHUB_REPOSITORY="Extra-Chill/homeboy-action"
export GITHUB_REF="refs/heads/main"
export GITHUB_SERVER_URL="https://github.com"
export GITHUB_RUN_ID="12345"
export COMPONENT_NAME="homeboy-action"
export AUTOFIX_BRANCH="ci/autofix/homeboy-action/main"
export GITHUB_OUTPUT="${TMP_DIR}/github-output"

source "${ROOT}/scripts/core/lib.sh"

OUTPUT_DIR="${TMP_DIR}/homeboy-output"
mkdir -p "${OUTPUT_DIR}"
cat > "${OUTPUT_DIR}/fix.json" <<'JSON'
{
  "data": {
    "collected_edits": [
      { "rule_id": "broken_doc_reference", "file": "docs/architecture/ci-results-contract.md" }
    ]
  }
}
JSON

SOURCE_REPORT="$(extract_fix_report_from_output "${OUTPUT_DIR}")"

REAL_REF_OUTPUT_DIR="${TMP_DIR}/real-refactor-output"
mkdir -p "${REAL_REF_OUTPUT_DIR}"
cat > "${REAL_REF_OUTPUT_DIR}/fix.json" <<'JSON'
{
  "success": false,
  "data": {
    "applied": true,
    "changed_files": [
      "src/core/code_audit/detectors/field_patterns.rs"
    ],
    "collected_edits": [
      {
        "action": "insert",
        "file": "src/core/code_audit/detectors/field_patterns.rs",
        "rule_id": "missingimport",
        "source": "audit"
      }
    ],
    "command": "refactor.sources",
    "files_modified": 1
  }
}
JSON

REAL_REF_REPORT="$(extract_fix_report_from_output "${REAL_REF_OUTPUT_DIR}")"
assert_contains "${REAL_REF_REPORT}" $'missingimport\t1\tsrc/core/code_audit/detectors/field_patterns.rs' "report extractor handles refactor.sources collected edits"

BODY_CAPTURE="${TMP_DIR}/source-create.md"
export BODY_CAPTURE
export HOMEBOY_PR_MODE="create"
export AUTOFIX_FILE_COUNT="2"
export AUTOFIX_TOTAL_FIXES="1"
export AUTOFIX_CHANGED_FILES=$'docs/architecture/ci-results-contract.md\nhomeboy.json'
export AUTOFIX_REPORT="${SOURCE_REPORT}"

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-source.log
SOURCE_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${SOURCE_BODY}" 'Fixed **1** `broken_doc_reference` finding.' "source summary includes finding type"
assert_contains "${SOURCE_BODY}" 'Changed **2** files.' "source summary includes changed file count"
assert_contains "${SOURCE_BODY}" '| `broken_doc_reference` | 1 | `docs/architecture/ci-results-contract.md` |' "source table includes category/count/file"
assert_contains "${SOURCE_BODY}" '- `homeboy.json` audit baseline metadata was refreshed.' "source report separates baseline churn"
assert_contains "${SOURCE_BODY}" 'Autofix branch pushed; use the PR branch checks as merge verification.' "verification points to PR branch checks"
assert_contains "${SOURCE_BODY}" '- Workflow run: https://github.com/Extra-Chill/homeboy-action/actions/runs/12345' "verification includes workflow run"
assert_not_contains "${SOURCE_BODY}" 'Opened immediately after autofix without rerunning quality gates.' "verification avoids stale no-gates wording"
assert_not_contains "${SOURCE_BODY}" 'file(s) fixed via' "source report omits generic old summary"

FALLBACK_OUTPUT_DIR="${TMP_DIR}/fallback-output"
mkdir -p "${FALLBACK_OUTPUT_DIR}"
cat > "${FALLBACK_OUTPUT_DIR}/fix.json" <<'JSON'
{
  "data": {
    "command": "refactor.sources",
    "applied": true,
    "changed_files": [
      "src/core/code_audit/duplication.rs",
      "src/core/code_audit/requested_detectors.rs"
    ],
    "collected_edits": []
  }
}
JSON

FALLBACK_REPORT="$(extract_fix_report_from_output "${FALLBACK_OUTPUT_DIR}")"
BODY_CAPTURE="${TMP_DIR}/fallback-create.md"
export BODY_CAPTURE
export HOMEBOY_PR_MODE="create"
export AUTOFIX_FILE_COUNT="2"
export AUTOFIX_TOTAL_FIXES="2"
export AUTOFIX_CHANGED_FILES=$'src/core/code_audit/duplication.rs\nsrc/core/code_audit/requested_detectors.rs'
export AUTOFIX_REPORT="${FALLBACK_REPORT}"

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-fallback.log
FALLBACK_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${FALLBACK_REPORT}" $'source_change\t2\tsrc/core/code_audit/duplication.rs, src/core/code_audit/requested_detectors.rs' "fallback report classifies refactor.sources changed files"
assert_contains "${FALLBACK_BODY}" 'Applied autofix changes to **2** source files.' "fallback summary treats source changes as fixes"
assert_contains "${FALLBACK_BODY}" '| `source_change` | 2 | `src/core/code_audit/duplication.rs` `src/core/code_audit/requested_detectors.rs` |' "fallback table includes changed source files"
assert_not_contains "${FALLBACK_BODY}" 'No source fixes were reported by the autofix output.' "fallback avoids false no-source-fixes summary"
assert_not_contains "${FALLBACK_BODY}" 'changed outside the reported source-fix set' "fallback does not misclassify source files as other changes"

BODY_CAPTURE="${TMP_DIR}/stale-source-report-create.md"
export BODY_CAPTURE
export AUTOFIX_FILE_COUNT="1"
export AUTOFIX_TOTAL_FIXES="2"
export AUTOFIX_CHANGED_FILES="src/core/code_audit/duplication.rs"
export AUTOFIX_REPORT="${FALLBACK_REPORT}"

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-stale-source-report.log
STALE_SOURCE_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${STALE_SOURCE_BODY}" 'Applied autofix changes to **1** source file.' "stale source report summary follows committed files"
assert_contains "${STALE_SOURCE_BODY}" '| `source_change` | 1 | `src/core/code_audit/duplication.rs` |' "stale source report table drops uncommitted files"
assert_not_contains "${STALE_SOURCE_BODY}" 'src/core/code_audit/requested_detectors.rs' "stale source report omits uncommitted source file"

LINT_FIX_OUTPUT_DIR="${TMP_DIR}/lint-fix-output"
mkdir -p "${LINT_FIX_OUTPUT_DIR}"
cat > "${LINT_FIX_OUTPUT_DIR}/fix.json" <<'JSON'
{
  "success": true,
  "data": {
    "autofix": {
      "changed_files": [
        "inc/wiki/class-intelligence-wiki-brain-coverage-planner.php"
      ],
      "files_modified": 1
    },
    "component": "intelligence",
    "status": "passed"
  }
}
JSON

LINT_FIX_REPORT="$(extract_fix_report_from_output "${LINT_FIX_OUTPUT_DIR}")"
assert_contains "${LINT_FIX_REPORT}" $'source_change	1	inc/wiki/class-intelligence-wiki-brain-coverage-planner.php' "lint --fix report classifies autofix changed files"

STAGE_REPO="${TMP_DIR}/stage-source-fallback"
mkdir -p "${STAGE_REPO}/src/Runtime"
git -C "${STAGE_REPO}" init -q
git -C "${STAGE_REPO}" config user.name "Test User"
git -C "${STAGE_REPO}" config user.email "test@example.com"
printf '{"component":"agents-api"}\n' > "${STAGE_REPO}/homeboy.json"
printf '<?php\nclass WP_Agent_Conversation_Loop {}\n' > "${STAGE_REPO}/src/Runtime/class-wp-agent-conversation-loop.php"
git -C "${STAGE_REPO}" add .
git -C "${STAGE_REPO}" commit -q -m "initial"
printf '{"component":"agents-api","baseline":true}\n' > "${STAGE_REPO}/homeboy.json"
printf '<?php\nclass WP_Agent_Conversation_Loop { }\n' > "${STAGE_REPO}/src/Runtime/class-wp-agent-conversation-loop.php"

export AUTOFIX_REPORT=""
STAGE_SOURCE_FILES="$(cd "${STAGE_REPO}" && autofix_stage_source_files "${STAGE_REPO}")"
assert_contains "${STAGE_SOURCE_FILES}" "src/Runtime/class-wp-agent-conversation-loop.php" "empty report falls back to changed source files"
assert_not_contains "${STAGE_SOURCE_FILES}" "homeboy.json" "empty report fallback excludes drift files"

BODY_CAPTURE="${TMP_DIR}/synthesized-create.md"
export BODY_CAPTURE
export AUTOFIX_FILE_COUNT="1"
export AUTOFIX_TOTAL_FIXES="1"
export AUTOFIX_CHANGED_FILES="src/core/code_audit/detectors/field_patterns.rs"
export AUTOFIX_REPORT=""

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-synthesized.log
SYNTHESIZED_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${SYNTHESIZED_BODY}" 'Applied autofix changes to **1** source file.' "synthesized report treats transported-empty fix as source change"
assert_contains "${SYNTHESIZED_BODY}" '| `source_change` | 1 | `src/core/code_audit/detectors/field_patterns.rs` |' "synthesized report includes changed source file"
assert_not_contains "${SYNTHESIZED_BODY}" 'changed outside the reported source-fix set' "synthesized report avoids unsafe other-change wording"

BODY_CAPTURE="${TMP_DIR}/unsafe-create.md"
export BODY_CAPTURE
export AUTOFIX_FILE_COUNT="1"
export AUTOFIX_TOTAL_FIXES="0"
export AUTOFIX_CHANGED_FILES="src/core/code_audit/detectors/field_patterns.rs"
export AUTOFIX_REPORT=""

UNSAFE_LOG="$(bash "${ROOT}/scripts/autofix/open-autofix-pr.sh")"

assert_contains "${UNSAFE_LOG}" 'Skipping unsafe autofix PR: changed files are not covered by the autofix report' "unsafe unreported source changes skip PR creation"
assert_contains "$(<"${GITHUB_OUTPUT}")" 'unsafe-unreported-changes=true' "unsafe skip writes output flag"

BODY_CAPTURE="${TMP_DIR}/baseline-create.md"
export BODY_CAPTURE
export AUTOFIX_FILE_COUNT="1"
export AUTOFIX_TOTAL_FIXES="0"
export AUTOFIX_CHANGED_FILES="homeboy.json"
export AUTOFIX_REPORT=""

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-baseline.log
BASELINE_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${BASELINE_BODY}" 'No source fixes were reported by the autofix output.' "baseline-only summary is explicit"
assert_contains "${BASELINE_BODY}" 'Changed **1** files.' "baseline-only summary includes changed count"
assert_contains "${BASELINE_BODY}" '- `homeboy.json` audit baseline metadata was refreshed.' "baseline-only report names metadata refresh"
assert_not_contains "${BASELINE_BODY}" '## Automated Fixes' "baseline-only report does not invent source fixes"

BODY_CAPTURE="${TMP_DIR}/source-edit.md"
export BODY_CAPTURE
export HOMEBOY_PR_MODE="find-existing"
export AUTOFIX_FILE_COUNT="1"
export AUTOFIX_TOTAL_FIXES="1"
export AUTOFIX_CHANGED_FILES="docs/architecture/ci-results-contract.md"
export AUTOFIX_REPORT="${SOURCE_REPORT}"

cat > "${FAKE_BIN}/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

body_file=""
is_edit="false"
for arg in "$@"; do
  [ "${arg}" = "edit" ] && is_edit="true"
done
while [ "$#" -gt 0 ]; do
  case "$1" in
    --body-file)
      body_file="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [ "${is_edit}" = "true" ]; then
  cp "${body_file}" "${BODY_CAPTURE}"
  printf '{"data":{"url":"https://github.com/Extra-Chill/homeboy-action/pull/42"}}\n'
else
  printf '{"data":{"items":[{"number":42,"url":"https://github.com/Extra-Chill/homeboy-action/pull/42"}]}}\n'
fi
SH
chmod +x "${FAKE_BIN}/homeboy"

EDIT_LOG="$(bash "${ROOT}/scripts/autofix/open-autofix-pr.sh")"
EDIT_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${EDIT_LOG}" 'Updated PR #42 body with latest run context' "existing PR update path edits body"
assert_contains "${EDIT_BODY}" '| `broken_doc_reference` | 1 | `docs/architecture/ci-results-contract.md` |' "existing PR body gets rich report"

printf 'All autofix PR report checks passed.\n'
