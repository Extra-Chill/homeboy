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

BODY_CAPTURE="${TMP_DIR}/source-create.md"
export BODY_CAPTURE
export HOMEBOY_PR_MODE="create"
export AUTOFIX_FILE_COUNT="2"
export AUTOFIX_CHANGED_FILES=$'docs/architecture/ci-results-contract.md\nhomeboy.json'
export AUTOFIX_REPORT="${SOURCE_REPORT}"

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-source.log
SOURCE_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${SOURCE_BODY}" 'Fixed **1** `broken_doc_reference` finding.' "source summary includes finding type"
assert_contains "${SOURCE_BODY}" 'Changed **2** files.' "source summary includes changed file count"
assert_contains "${SOURCE_BODY}" '| `broken_doc_reference` | 1 | `docs/architecture/ci-results-contract.md` |' "source table includes category/count/file"
assert_contains "${SOURCE_BODY}" '- `homeboy.json` audit baseline metadata was refreshed.' "source report separates baseline churn"
assert_contains "${SOURCE_BODY}" '- Workflow run: https://github.com/Extra-Chill/homeboy-action/actions/runs/12345' "verification includes workflow run"
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
export AUTOFIX_CHANGED_FILES=$'src/core/code_audit/duplication.rs\nsrc/core/code_audit/requested_detectors.rs'
export AUTOFIX_REPORT="${FALLBACK_REPORT}"

bash "${ROOT}/scripts/autofix/open-autofix-pr.sh" >/tmp/homeboy-action-test-fallback.log
FALLBACK_BODY="$(<"${BODY_CAPTURE}")"

assert_contains "${FALLBACK_REPORT}" $'source_change\t2\tsrc/core/code_audit/duplication.rs, src/core/code_audit/requested_detectors.rs' "fallback report classifies refactor.sources changed files"
assert_contains "${FALLBACK_BODY}" 'Applied autofix changes to **2** source files.' "fallback summary treats source changes as fixes"
assert_contains "${FALLBACK_BODY}" '| `source_change` | 2 | `src/core/code_audit/duplication.rs` `src/core/code_audit/requested_detectors.rs` |' "fallback table includes changed source files"
assert_not_contains "${FALLBACK_BODY}" 'No source fixes were reported by the autofix output.' "fallback avoids false no-source-fixes summary"
assert_not_contains "${FALLBACK_BODY}" 'changed outside the reported source-fix set' "fallback does not misclassify source files as other changes"

BODY_CAPTURE="${TMP_DIR}/baseline-create.md"
export BODY_CAPTURE
export AUTOFIX_FILE_COUNT="1"
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
