#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT
mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/output"

cat > "${TMP_DIR}/bin/gh" <<'SH'
#!/usr/bin/env bash
printf 'OPEN\n'
SH
cat > "${TMP_DIR}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == 'git pr comment --help' ]]; then
  if [ "${SUPPORTS_GUARD}" = true ]; then
    echo 'Options: --expected-head-sha <SHA>'
  else
    echo 'Options: --number <NUMBER>'
  fi
  exit 0
fi
if [[ "$*" == 'git pr comment '* ]]; then
  printf '%s\n' "$@" > "${COMMENT_ARGS}"
  if [ "${SUPPORTS_GUARD}" = false ] && [[ "$*" == *--expected-head-sha* ]]; then
    exit 1
  fi
  printf '%s\n' "${COMMENT_RESULT}"
fi
SH
chmod +x "${TMP_DIR}/bin/gh" "${TMP_DIR}/bin/homeboy"

export PATH="${TMP_DIR}/bin:${PATH}"
export GITHUB_ACTION_PATH="${ROOT}"
export HOMEBOY_OUTPUT_DIR="${TMP_DIR}/output"
export GITHUB_ENV="${TMP_DIR}/github.env"
export COMMENT_ARGS="${TMP_DIR}/args"
export GITHUB_REPOSITORY="Extra-Chill/homeboy"
export GITHUB_WORKFLOW="CI" GITHUB_JOB="test"
export GH_TOKEN="app-token" PR_NUMBER="123" HEAD_SHA="run-sha"
export COMPONENT_NAME="homeboy" COMMANDS="review lint"
export RESULTS='{"review lint":"fail"}'
export SUPPORTS_GUARD=true
export COMMENT_RESULT='{"data":{"action":"created","comment_id":123}}'

run_comment() {
  : > "${GITHUB_ENV}"
  : > "${COMMENT_ARGS}"
  bash "${ROOT}/scripts/pr/post-pr-comment.sh" > "${TMP_DIR}/log"
}

run_comment
grep -Fxq -- '--expected-head-sha' "${COMMENT_ARGS}"
grep -Fxq -- 'run-sha' "${COMMENT_ARGS}"
grep -Fq 'HOMEBOY_PR_COMMENT_POSTED=true' "${GITHUB_ENV}"

export SUPPORTS_GUARD=false
run_comment
! grep -Fq -- '--expected-head-sha' "${COMMENT_ARGS}"
grep -Fq 'PR comment posted successfully' "${TMP_DIR}/log"
grep -Fq 'HOMEBOY_PR_COMMENT_POSTED=true' "${GITHUB_ENV}"

export SUPPORTS_GUARD=true
# Even if a skipped result contains an ID, it must never be marked posted.
export COMMENT_RESULT='{"data":{"action":"skipped","skip_reason":"stale_head","current_head_sha":"new-sha","comment_id":123}}'
run_comment
grep -Fq 'Skipped PR comment: stale_head (run run-sha, PR head new-sha)' "${TMP_DIR}/log"
! grep -Fq 'posted successfully' "${TMP_DIR}/log"
test ! -s "${GITHUB_ENV}"

export COMMENT_RESULT='{"data":{"action":"skipped","skip_reason":"pr_not_open"}}'
run_comment
grep -Fq 'Skipped PR comment: pr_not_open' "${TMP_DIR}/log"
test ! -s "${GITHUB_ENV}"

printf 'All expected-head-sha PR comment checks passed.\n'
