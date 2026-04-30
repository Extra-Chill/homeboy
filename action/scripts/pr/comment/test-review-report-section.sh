#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

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

fake_bin="$(mktemp -d)"
trap 'rm -rf "${fake_bin}"' EXIT

cat > "${fake_bin}/homeboy" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${HOMEBOY_REVIEW_CALL_LOG}"
printf ':zap: Scope: **changed files only** (since `origin/main`)\n\n'
printf '> :wrench: **autofix:** applied — 2 file(s) fixed via lint\n'
printf '> :warning: **binary-source:** fallback release binary (source build failed)\n\n'
printf '**2** finding(s) across 2 stage(s)\n\n'
printf ':x: **audit** — failed (2 finding(s))\n'
exit 1
SH
chmod +x "${fake_bin}/homeboy"

export PATH="${fake_bin}:${PATH}"
export HOMEBOY_REVIEW_CALL_LOG="${fake_bin}/review.log"
export GITHUB_ACTION_PATH="${ROOT}"
export COMMANDS="audit,lint,test"
export RESULTS='{"audit":"fail","lint":"pass","test":"pass"}'
export COMP_ID="data-machine"
export WORKSPACE="/tmp/workspace"
export SECTION_TITLE="Audit"
export AUTOFIX_ENABLED="true"
export AUTOFIX_COMMITTED="true"
export AUTOFIX_FILE_COUNT="2"
export AUTOFIX_FIX_TYPES="lint"
export BINARY_SOURCE="fallback"
export SCOPE_MODE="changed"
export SCOPE_BASE_REF="origin/main"
export DIGEST_FILE=""

source "${ROOT}/scripts/core/lib.sh"
source "${ROOT}/scripts/pr/comment/sections.sh"

build_section_body

assert_contains "${SECTION_BODY}" "### Audit" "section title preserved"
assert_contains "${SECTION_BODY}" "**2** finding(s) across 2 stage(s)" "review markdown appended"
assert_contains "${SECTION_BODY}" ":x: **audit** — failed" "review stage markdown appended"
assert_not_contains "${SECTION_BODY}" ":x: **audit** _(changed files only)_" "legacy per-command audit block skipped"
assert_contains "$(cat "${HOMEBOY_REVIEW_CALL_LOG}")" "--banner autofix=applied — 2 file(s) fixed via lint" "autofix banner passed to core"
assert_contains "$(cat "${HOMEBOY_REVIEW_CALL_LOG}")" "--banner binary-source=fallback release binary (source build failed)" "binary-source banner passed to core"

export EXTRA_ARGS="--format json"
build_section_body

assert_contains "${SECTION_BODY}" ":x: **audit** — failed" "custom args render split audit status"
assert_contains "${SECTION_BODY}" ":white_check_mark: **lint** — passed" "custom args render split lint status"
assert_contains "${SECTION_BODY}" ":white_check_mark: **test** — passed" "custom args render split test status"
assert_not_contains "${SECTION_BODY}" ":x: **audit** _(changed files only)_" "custom args do not fall back to legacy command blocks"
assert_not_contains "${SECTION_BODY}" "core PR-comment rendering currently supports the default" "custom args do not use unsupported warning"

split_output_dir="$(mktemp -d)"
cat > "${split_output_dir}/lint.json" <<'JSON'
{
  "success": false,
  "data": {
    "hints": ["Some issues may require manual fixes"],
    "lint_findings": [
      {"category":"phpstan","message":"A"},
      {"category":"phpstan","message":"B"},
      {"category":"phpcs","message":"C"}
    ]
  }
}
JSON

export COMMANDS="lint"
export RESULTS='{"lint":"fail"}'
export SECTION_TITLE="Lint"
export OUTPUT_DIR="${split_output_dir}"
unset EXTRA_ARGS
build_section_body

assert_contains "${SECTION_BODY}" "### Lint" "split command section title preserved"
assert_contains "${SECTION_BODY}" ":x: **lint** — failed" "split command failure rendered"
assert_contains "${SECTION_BODY}" '`phpstan` — 2 finding(s)' "split command phpstan bucket rendered"
assert_contains "${SECTION_BODY}" '`phpcs` — 1 finding(s)' "split command phpcs bucket rendered"
assert_contains "${SECTION_BODY}" "_Total: 3 finding(s)_" "split command total rendered"
assert_contains "${SECTION_BODY}" "Some issues may require manual fixes" "split command hints rendered"
assert_contains "${SECTION_BODY}" "Deep dive: homeboy lint data-machine --changed-since origin/main" "split command deep dive rendered"
assert_not_contains "${SECTION_BODY}" "core PR-comment rendering currently supports the default" "split command does not use unsupported warning"

printf 'All review report section checks passed.\n'
