#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROGRESS="${ROOT_DIR}/scripts/core/phase-progress.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

set +e
GITHUB_REPOSITORY='Extra-Chill/homeboy' \
GITHUB_RUN_ID='1234' \
GITHUB_JOB='test' \
GITHUB_OUTPUT="${TMP_DIR}/output" \
GITHUB_STEP_SUMMARY="${TMP_DIR}/summary" \
HOMEBOY_CI_RESULTS_DIR="${TMP_DIR}/results" \
HOMEBOY_ACTION_PHASE_PROGRESS_FILE="${TMP_DIR}/phases.jsonl" \
HOMEBOY_ACTION_PHASE_HEARTBEAT_SECONDS=1 \
HOMEBOY_ACTION_PHASE_BUDGET_SECONDS=1 \
bash "${PROGRESS}" run command_execution -- bash -c 'sleep 2' >"${TMP_DIR}/run.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 0 ]; then
  printf 'FAIL: delayed phase exits %s\n' "${exit_code}"
  exit 1
fi
if ! grep -q 'phase heartbeat.*command_execution running' "${TMP_DIR}/run.log"; then
  printf 'FAIL: delayed phase did not emit a heartbeat before completion\n'
  exit 1
fi
if ! grep -q 'phase budget exceeded.*Homeboy command runner' "${TMP_DIR}/run.log"; then
  printf 'FAIL: delayed phase did not name an owning subsystem after its budget\n'
  exit 1
fi

GITHUB_REPOSITORY='Extra-Chill/homeboy' \
GITHUB_RUN_ID='1234' \
GITHUB_JOB='test' \
GITHUB_OUTPUT="${TMP_DIR}/output" \
GITHUB_STEP_SUMMARY="${TMP_DIR}/summary" \
HOMEBOY_CI_RESULTS_DIR="${TMP_DIR}/results" \
HOMEBOY_ACTION_PHASE_PROGRESS_FILE="${TMP_DIR}/phases.jsonl" \
bash "${PROGRESS}" summary >/dev/null

if ! jq -e '.run_ref == "github://Extra-Chill/homeboy/actions/runs/1234#test" and .slowest_phases[0].phase == "command_execution"' "${TMP_DIR}/results/phase-progress.json" >/dev/null; then
  printf 'FAIL: phase artifact does not preserve the stable run ref and slowest phase\n'
  exit 1
fi
if ! grep -q 'Three slowest phases' "${TMP_DIR}/summary"; then
  printf 'FAIL: phase summary does not report the slowest phases\n'
  exit 1
fi
printf 'PASS: delayed phase emits live progress before terminal timing evidence\n'
