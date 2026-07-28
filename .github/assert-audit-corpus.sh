#!/usr/bin/env bash
#
# Assert that the post-merge full-tree audit actually READ the tree.
#
# This is the blocking half of `full-audit-gate` in audit-debt.yml. It exists
# because #10557 was a gate that reported success while measuring nothing:
# `review audit` scanned 0 of 1817 files and passed in 7 seconds.
#
# It lives in a script rather than inline in the workflow so it can be executed
# by `tests/audit_debt_workflow_test.rs` against recorded fixtures. The two
# defects this file replaces were both invisible to a YAML-substring test:
#
#   1. It read `${HOMEBOY_OUTPUT_DIR}/audit.json`. homeboy-action names the
#      result file after the command it ran — `command_output_stem "review
#      audit"` is `review-audit` — so the file it looked for never existed and
#      the assertion failed 100% of the time, on every merge, from the moment
#      it landed.
#   2. #10583 then "fixed" the jq path to `.data.audit.output.summary`. That is
#      the shape of the `review` UMBRELLA command (audit+lint+test stages).
#      `review audit` on its own returns the flat `AuditCommandOutput`, so the
#      corpus figure is at `.data.summary.files_scanned`. Defect 1 masked
#      defect 2: the file was never opened, so the wrong path never showed.
#
# A gate whose test cannot detect that it checks nothing is not a gate. The
# accompanying tests run THIS script against a recorded `review audit --output`
# payload plus one fixture per historical defect, so neither can return.
#
# Inputs (environment):
#   HOMEBOY_OUTPUT_DIR       — set by homeboy-action's run-homeboy-commands.sh
#   AUDIT_MIN_FILES_SCANNED  — sanity floor, not a target (default 500)
#
# Exit 0 only when a real audit result was read and its corpus is plausible.
# Every other outcome is exit 1: this gate is allowed to produce a false RED,
# never a false green.

set -euo pipefail

OUTPUT_DIR="${HOMEBOY_OUTPUT_DIR:-}"
MIN_FILES="${AUDIT_MIN_FILES_SCANNED:-500}"

# Must match homeboy-action `command_output_stem "review audit"`.
RESULT_BASENAME="review-audit.json"
RESULT="${OUTPUT_DIR}/${RESULT_BASENAME}"

fail() {
  echo "::error::$*" >&2
  exit 1
}

if ! command -v jq >/dev/null 2>&1; then
  fail "jq is not installed, so this gate cannot read the audit result. Failing closed rather than assuming the audit passed. See #10557."
fi

if [ -z "${OUTPUT_DIR}" ]; then
  fail "HOMEBOY_OUTPUT_DIR is unset. homeboy-action exports it to \$GITHUB_ENV from run-homeboy-commands.sh; if that moved, this gate can no longer read the audit result and must not claim the audit passed. See #10557."
fi

if [ ! -d "${OUTPUT_DIR}" ]; then
  fail "HOMEBOY_OUTPUT_DIR '${OUTPUT_DIR}' is not a directory. See #10557."
fi

if [ ! -s "${RESULT}" ]; then
  echo "contents of ${OUTPUT_DIR}:" >&2
  ls -la "${OUTPUT_DIR}" >&2 || true
  fail "Audit produced no structured output at '${RESULT}'. homeboy-action names this file after the command it ran (\`command_output_stem 'review audit'\` -> '${RESULT_BASENAME}'), so a rename on either side lands here — that is exactly how this assertion spent its whole life failing on a file called 'audit.json'. Compare the directory listing above against the expected name. See #10557."
fi

# `jq -e` (not `// 0`): a MISSING field means the result shape changed, which is
# a different failure from "the audit scanned nothing". Conflating them is how
# #10583 shipped a wrong jq path and reported it as an empty corpus.
if ! files_scanned="$(jq -er '.data.summary.files_scanned' "${RESULT}" 2>/dev/null)"; then
  echo "top-level result summary:" >&2
  jq -r '{success, exit_code, status, summary, diagnostics: (.diagnostics.code // null)}' "${RESULT}" >&2 || true
  fail "'${RESULT}' has no '.data.summary.files_scanned'. Either \`review audit\` failed before producing an audit (read the summary above — a stale baseline row or a config error lands here) or its output shape changed. Note '.data.audit.output.summary' is the shape of the \`review\` UMBRELLA command; \`review audit\` alone returns the flat audit output. See #10557, #10583."
fi

case "${files_scanned}" in
  '' | *[!0-9]*)
    fail "'${RESULT}' reported a non-numeric corpus size '${files_scanned}'. See #10557."
    ;;
esac

echo "audit corpus: ${files_scanned} file(s) (floor ${MIN_FILES})"

if [ "${files_scanned}" -lt "${MIN_FILES}" ]; then
  jq -r '.data.summary' "${RESULT}" >&2 || true
  fail "Audit scanned ${files_scanned} files, below the ${MIN_FILES}-file sanity floor for this repository (~1600 scannable sources). An audit with a collapsed corpus has not passed — it has not run. The usual cause is a missing fingerprinting extension: this gate must go through Extra-Chill/homeboy-action, whose 'Install extension' step installs the extensions declared in homeboy.json. If the tree legitimately shrank, lower AUDIT_MIN_FILES_SCANNED in audit-debt.yml deliberately. See #10557."
fi
