#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/core/run-baseline-commands.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/workspace"
cat > "${TMP_DIR}/bin/homeboy" <<'SH'
#!/usr/bin/env bash

set -euo pipefail

output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    *) shift ;;
  esac
done

mkdir -p "$(dirname "${output}")"
case "${FAKE_HOMEBOY_MODE:-}" in
  missing) : ;;
  malformed) printf '%s\n' 'not json' > "${output}" ;;
  *) printf '%s\n' '{"success":true}' > "${output}" ;;
esac

case "${FAKE_HOMEBOY_MODE:-}" in
  descendant)
    sleep 30 &
    echo "$!" > "${FAKE_HOMEBOY_CHILD_PID_FILE}"
    printf 'baseline output retained\n'
    ;;
  timeout)
    sleep 30
    ;;
esac
SH
chmod +x "${TMP_DIR}/bin/homeboy"

cd "${TMP_DIR}/workspace"
git init -q -b main
git config user.email test@example.com
git config user.name test
touch fixture
git add fixture
git commit -qm fixture
git checkout -qb feature

run_baseline() {
  local mode="$1"
  local log_file="$2"
  local env_file="${TMP_DIR}/github-env-${mode}"

  set +e
  PATH="${TMP_DIR}/bin:${PATH}" \
  GITHUB_ACTION_PATH="${ROOT_DIR}" \
  GITHUB_WORKSPACE="${TMP_DIR}/workspace" \
  GITHUB_ENV="${env_file}" \
  COMMANDS='review test' \
  COMPONENT_NAME='fixture' \
  BASELINE_COMMANDS='auto' \
  HOMEBOY_DIFFERENTIAL_GATING=true \
  SCOPE_CONTEXT=pr \
  SCOPE_BASE_REF=main \
  HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=1 \
  HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS=1 \
  RUN_GROUP_PREFIX='baseline test' \
  FAKE_HOMEBOY_MODE="${mode}" \
  FAKE_HOMEBOY_CHILD_PID_FILE="${TMP_DIR}/child-${mode}.pid" \
  bash "${RUNNER}" >"${log_file}" 2>&1
  local exit_code=$?
  set -e

  if [ "${exit_code}" -ne 0 ]; then
    printf 'FAIL: baseline runner exits 0 to preserve differential result processing, got %s\n' "${exit_code}"
    exit 1
  fi

  local output_dir
  output_dir="$(awk -F= '/^HOMEBOY_BASE_OUTPUT_DIR=/{print $2}' "${env_file}")"
  printf '%s\n' "${output_dir}"
}

descendant_log="${TMP_DIR}/descendant.log"
descendant_output_dir="$(run_baseline descendant "${descendant_log}")"
child_pid="$(<"${TMP_DIR}/child-descendant.pid")"
if kill -0 "${child_pid}" 2>/dev/null; then
  printf 'FAIL: baseline finalization leaves descendant process %s alive\n' "${child_pid}"
  exit 1
fi
if ! grep -q 'finalization terminated surviving command containment' "${descendant_log}"; then
  printf 'FAIL: baseline finalization did not report descendant cleanup\n'
  exit 1
fi
if ! grep -q 'baseline output retained' "${descendant_log}"; then
  printf 'FAIL: baseline did not emit retained command output after finalization\n'
  exit 1
fi
if ! jq -e '."review test" | .status == "pass" and .exit_code == 0 and .structured_output == true' "${descendant_output_dir}/baseline-status.json" >/dev/null; then
  printf 'FAIL: descendant cleanup changed a successful baseline result\n'
  exit 1
fi
printf 'PASS: baseline finalization cleans descendants and preserves successful results\n'

timeout_log="${TMP_DIR}/timeout.log"
timeout_output_dir="$(run_baseline timeout "${timeout_log}")"
if ! grep -q 'baseline homeboy review test exceeded its 1s execution timeout' "${timeout_log}"; then
  printf 'FAIL: baseline timeout did not emit actionable liveness evidence\n'
  exit 1
fi
if ! jq -e '."review test" | .status == "timeout" and .exit_code == 124 and .structured_output == true' "${timeout_output_dir}/baseline-status.json" >/dev/null; then
  printf 'FAIL: baseline timeout did not preserve differential failure semantics\n'
  exit 1
fi
printf 'PASS: baseline timeout records actionable failure evidence\n'

for output_mode in missing malformed; do
  output_log="${TMP_DIR}/${output_mode}.log"
  output_dir="$(run_baseline "${output_mode}" "${output_log}")"
  if ! jq -e '."review test" | .status == "fail" and .exit_code == 0 and .structured_output == false' "${output_dir}/baseline-status.json" >/dev/null || ! grep -q 'did not write valid structured output' "${output_log}"; then
    printf 'FAIL: zero-exit baseline %s structured output fails closed\n' "${output_mode}"
    exit 1
  fi
done
printf 'PASS: zero-exit baseline missing and malformed structured output fail closed\n'
