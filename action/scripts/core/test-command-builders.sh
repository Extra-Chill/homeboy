#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"

assert_equals() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if [ "${expected}" != "${actual}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual:   %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  case "${haystack}" in
    *"${needle}"*)
      printf 'PASS: %s\n' "${label}"
      ;;
    *)
      printf 'FAIL: %s\nexpected to contain: %s\nactual:              %s\n' "${label}" "${needle}" "${haystack}"
      exit 1
      ;;
  esac
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  case "${haystack}" in
    *"${needle}"*)
      printf 'FAIL: %s\nexpected not to contain: %s\nactual:                  %s\n' "${label}" "${needle}" "${haystack}"
      exit 1
      ;;
    *)
      printf 'PASS: %s\n' "${label}"
      ;;
  esac
}

WORKSPACE="/tmp/workspace"
COMPONENT="data-machine"
OUTPUT_JSON="/tmp/workspace/out.json"

# ── Unscoped (full mode) ──
unset GITHUB_ACTIONS SCOPE_MODE SCOPE_BASE_REF EXTRA_ARGS || true
SCOPE_MODE="full"
assert_equals \
  "homeboy review lint data-machine --path /tmp/workspace" \
  "$(build_run_command "review lint" "${COMPONENT}" "${WORKSPACE}")" \
  "review lint includes workspace path"

# ── Scoped (changed mode) ──
SCOPE_MODE="changed"
SCOPE_BASE_REF="origin/main"
unset HOMEBOY_DIFFERENTIAL_GATING || true
assert_equals \
  "homeboy review lint data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "review lint" "${COMPONENT}" "${WORKSPACE}")" \
  "review lint keeps path with changed-since"

GITHUB_ACTIONS="true"
assert_equals \
  "homeboy --force-hot --allow-local-hot --output /tmp/workspace/out.json review lint data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "review lint" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "GitHub Actions review lint opts into hot-runner execution"

assert_equals \
  "homeboy --force-hot --allow-local-hot review test data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "review test" "${COMPONENT}" "${WORKSPACE}")" \
  "GitHub Actions review test opts into hot-runner execution"

unset GITHUB_ACTIONS

assert_equals \
  "homeboy review audit data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "review audit" "${COMPONENT}" "${WORKSPACE}")" \
  "review audit keeps path with changed-since"

HOMEBOY_DIFFERENTIAL_GATING="true"
assert_equals \
  "homeboy review audit data-machine --path /tmp/workspace" \
  "$(build_run_command "review audit" "${COMPONENT}" "${WORKSPACE}")" \
  "differential review audit uses full scope"

assert_equals \
  "homeboy review test data-machine --path /tmp/workspace" \
  "$(build_run_command "review test" "${COMPONENT}" "${WORKSPACE}")" \
  "differential review test uses full scope"

assert_equals \
  "homeboy review lint data-machine --path /tmp/workspace --changed-since origin/main" \
  "$(build_run_command "review lint" "${COMPONENT}" "${WORKSPACE}")" \
  "differential review lint keeps changed scope"

unset HOMEBOY_DIFFERENTIAL_GATING

assert_equals \
  "homeboy review data-machine --path /tmp/workspace --report=pr-comment --changed-since origin/main" \
  "$(build_review_report_command "${COMPONENT}" "${WORKSPACE}")" \
  "review report keeps path with changed-since"

assert_equals \
  "review audit,review lint,review test" \
  "$(resolve_baseline_commands "review audit,review lint,review test" "auto")" \
  "baseline auto keeps requested review audit/lint/test commands"

assert_equals \
  "review audit" \
  "$(resolve_baseline_commands "review audit,review lint,review test" "review audit")" \
  "baseline subset uses requested review command shape"

assert_equals \
  "" \
  "$(resolve_baseline_commands "review test" "review audit")" \
  "baseline subset intersects with matrix command"

assert_equals \
  "" \
  "$(resolve_baseline_commands "review audit,review lint,review test" "none")" \
  "baseline none disables differential reruns"

EXTRA_ARGS="--format json"
assert_equals \
  "homeboy review audit data-machine --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "review audit" "${COMPONENT}" "${WORKSPACE}")" \
  "run command appends extra args"

assert_equals \
  "homeboy --output /tmp/workspace/out.json review audit data-machine --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "review audit" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "run command keeps output path before extra args"

unset EXTRA_ARGS
BENCH_RIG="main,pr"
BENCH_SCENARIO="pipeline-scale"
BENCH_RUNS="3"
BENCH_ITERATIONS="10"
BENCH_REGRESSION_THRESHOLD="5"
assert_equals \
  "homeboy --output /tmp/workspace/out.json bench data-machine --path /tmp/workspace --rig main,pr --scenario pipeline-scale --runs 3 --iterations 10 --regression-threshold 5" \
  "$(build_run_command "bench" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "bench includes first-class benchmark flags"
unset BENCH_RIG BENCH_SCENARIO BENCH_RUNS BENCH_ITERATIONS BENCH_REGRESSION_THRESHOLD

HOMEBOY_SETTINGS_JSON='{"wp_codebox_workloads":[{"id":"ssi-import","run":[{"type":"wp-cli","command":"wp static-site-importer import-theme static-sites/demo/index.html --format=json","parse":"json"}]}],"wp_codebox_blueprint":{"preferredVersions":{"php":"8.3","wp":"latest"}}}'
bench_settings_cmd="$(build_run_command "bench" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")"
assert_contains "${bench_settings_cmd}" "--setting-json wp_codebox_workloads=" "bench forwards WP Codebox workloads as typed settings"
assert_contains "${bench_settings_cmd}" "--setting-json wp_codebox_blueprint=" "bench forwards WP Codebox blueprint as typed settings"
assert_not_contains "${bench_settings_cmd}" "HOMEBOY_SETTINGS_JSON" "bench does not pass raw settings env name as argument"
unset HOMEBOY_SETTINGS_JSON
EXTRA_ARGS="--format json"

assert_equals \
  "homeboy refactor data-machine --from lint --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from lint --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor lint keeps path and changed-since"

assert_equals \
  "homeboy --output /tmp/workspace/out.json refactor data-machine --from lint --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from lint --write" "${COMPONENT}" "${WORKSPACE}" "${OUTPUT_JSON}")" \
  "autofix refactor lint keeps output path and changed-since"

assert_equals \
  "homeboy refactor data-machine --from audit --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from audit --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor audit keeps path and changed-since"

assert_equals \
  "homeboy refactor data-machine --all --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_run_command "refactor --all" "${COMPONENT}" "${WORKSPACE}")" \
  "refactor keeps path with changed-since"

assert_equals \
  "refactor---all" \
  "$(command_output_stem "refactor --all")" \
  "output stem sanitizes spaced refactor command"

assert_equals \
  "refactor---from-audit---write" \
  "$(command_output_stem "refactor --from audit --write")" \
  "output stem sanitizes autofix command"

assert_equals \
  "homeboy refactor data-machine --from-audit --from-lint --from-test --all --write --path /tmp/workspace --changed-since origin/main --format json" \
  "$(build_autofix_command "refactor --from-audit --from-lint --from-test --all --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor keeps path and changed-since"

# ── Unscoped autofix ──
unset SCOPE_MODE SCOPE_BASE_REF EXTRA_ARGS || true
SCOPE_MODE="full"
assert_equals \
  "homeboy refactor data-machine --from test --write --path /tmp/workspace" \
  "$(build_autofix_command "refactor --from test --write" "${COMPONENT}" "${WORKSPACE}")" \
  "autofix refactor test keeps workspace path"

assert_equals \
  "homeboy refactor data-machine --all --path /tmp/workspace" \
  "$(build_run_command "refactor --all" "${COMPONENT}" "${WORKSPACE}")" \
  "refactor keeps workspace path"

assert_equals \
  "homeboy review data-machine --path /tmp/workspace --report=pr-comment" \
  "$(build_review_report_command "${COMPONENT}" "${WORKSPACE}")" \
  "review report keeps workspace path"

assert_equals \
  "homeboy runs export --since 24h --output /tmp/workspace/homeboy-observations" \
  "$(build_observation_export_command "24h" "/tmp/workspace/homeboy-observations")" \
  "observation export uses separate output directory"

assert_equals \
  "homeboy runs import /tmp/workspace/homeboy-observations-import/job" \
  "$(build_observation_import_command "/tmp/workspace/homeboy-observations-import/job")" \
  "observation import uses downloaded bundle directory"

PR_HEAD_REPO="some-contributor/homeboy-action"
GITHUB_REPOSITORY="Extra-Chill/homeboy-action"
GITHUB_HEAD_REF="feat/fork-pr"
GITHUB_REF_NAME="ignored-here"
assert_equals \
  "some-contributor/homeboy-action" \
  "$(resolve_pr_target_repo)" \
  "target repo prefers PR head repo"

assert_equals \
  "feat/fork-pr" \
  "$(resolve_pr_target_branch)" \
  "target branch prefers PR head ref"

assert_equals \
  "https://github.com/some-contributor/homeboy-action.git" \
  "$(build_github_remote_url "some-contributor/homeboy-action")" \
  "build github remote url without token"

assert_equals \
  "https://x-access-token:secret123@github.com/some-contributor/homeboy-action.git" \
  "$(build_github_remote_url "some-contributor/homeboy-action" "secret123")" \
  "build github remote url with token"

assert_equals \
  "origin" \
  "$(resolve_push_target "Extra-Chill/homeboy-action")" \
  "same-repo push without token uses origin"

assert_equals \
  "https://github.com/some-contributor/homeboy-action.git" \
  "$(resolve_push_target "some-contributor/homeboy-action")" \
  "fork push without token uses explicit remote url"

assert_equals \
  "https://x-access-token:secret123@github.com/some-contributor/homeboy-action.git" \
  "$(resolve_push_target "some-contributor/homeboy-action" "secret123")" \
  "fork push with token uses authenticated remote url"

# ── Canonicalize: fleet/deploy commands are filtered out ──

assert_equals \
  "review audit,review lint,review test" \
  "$(canonicalize_commands "review audit,review lint,review test,fleet exec my-fleet -- homeboy upgrade")" \
  "canonicalize strips fleet commands"

assert_equals \
  "review audit,review lint,review test" \
  "$(canonicalize_commands "review audit,deploy my-project --all,review lint,review test")" \
  "canonicalize strips deploy commands"

assert_equals \
  "review audit,review lint,review test,refactor --all" \
  "$(canonicalize_commands "deploy --fleet prod data-machine,review audit,review lint,review test,fleet status my-fleet,refactor --all")" \
  "canonicalize strips all operations and preserves order"

assert_equals \
  "review audit,review lint,review test,refactor --all,bench" \
  "$(canonicalize_commands "bench,review audit,review lint,review test,refactor --all")" \
  "canonicalize places bench after quality commands"

assert_equals \
  "review audit,review lint,review test" \
  "$(canonicalize_commands "review test,review audit,review lint")" \
  "canonicalize orders review quality commands"

assert_equals \
  "" \
  "$(canonicalize_commands "fleet exec my-fleet -- homeboy upgrade,deploy my-project --all")" \
  "canonicalize returns empty when only operations commands"

assert_equals \
  "review audit,review lint,review test" \
  "$(canonicalize_commands "release,review audit,review lint,review test")" \
  "canonicalize strips release commands"

assert_equals \
  "" \
  "$(canonicalize_commands "release")" \
  "canonicalize returns empty when only release commands"

printf 'All command builder checks passed.\n'
