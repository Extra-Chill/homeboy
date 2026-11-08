#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
export GITHUB_ACTION_PATH="${ROOT}"
source "${ROOT}/scripts/core/lib.sh"
source "${ROOT}/scripts/pr/comment/sections.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
mkdir -p "${TMP}/candidate" "${TMP}/base"
export OUTPUT_DIR="${TMP}/candidate" COMMANDS='review lint' SCOPE_MODE=full TEST_SCOPE_EFFECTIVE=full
export COMP_ID=booking GITHUB_WORKFLOW=Homeboy GITHUB_JOB=component-ci
[ "$(derive_comment_key)" = Homeboy ]
[ "$(derive_section_key)" = booking ]
COMP_ID=api
[ "$(derive_comment_key)" = Homeboy ]
[ "$(derive_section_key)" = api ]
COMMENT_KEY_INPUT=custom
[ "$(derive_comment_key)" = custom ]
unset COMMENT_KEY_INPUT
GITHUB_JOB=lint
[ "$(derive_comment_key)" = Homeboy:api ]
[ "$(derive_section_key)" = review-lint ]
GITHUB_JOB=component-ci
SECTION_TITLE=booking
printf '{"summary":{"findings":1405}}' > "${TMP}/base/review-lint.json"
printf '{"summary":{"findings":1405}}' > "${OUTPUT_DIR}/review-lint.json"
RESULTS="$(python3 "${ROOT}/scripts/core/apply-differential-gate.py" '{"review lint":"fail"}' "${OUTPUT_DIR}" "${TMP}/base")"
[ "${RESULTS}" = '{"review lint":"baseline_red"}' ]
if comment_has_actionable_content; then echo 'FAIL: differential-green should skip'; exit 1; fi
build_section_body
[[ "${SECTION_BODY}" == *':white_check_mark:'* && "${SECTION_BODY}" == *'no new findings'* && "${SECTION_BODY}" != *':x:'* ]]
printf '{"summary":{"findings":1407}}' > "${OUTPUT_DIR}/review-lint.json"
RESULTS="$(python3 "${ROOT}/scripts/core/apply-differential-gate.py" '{"review lint":"fail"}' "${OUTPUT_DIR}" "${TMP}/base")"
comment_has_actionable_content
build_section_body
[[ "${SECTION_BODY}" == *':x:'* && "${SECTION_BODY}" == *'2 new finding(s)'* && "${SECTION_BODY}" != *'1407'* ]]
[[ "${SECTION_BODY}" == *'<details><summary>Baseline: 1405'* ]]
# Missing/unmeasured evidence must not become a clean verdict.
printf '{"commands":{},"schema":"homeboy/differential-comment/v1"}' > "${OUTPUT_DIR}/differential-comment.json"
RESULTS='{"review lint":"inconclusive"}'
comment_has_actionable_content
printf 'PASS: matrix keys and differential comment verdicts\n'
