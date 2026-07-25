#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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
printf '%s\n' '{"success":false,"data":{"test_counts":{"failed":2,"passed":41,"total":43}}}' > "${output}"
exit 1
SH
chmod +x "${TMP_DIR}/bin/homeboy"

set +e
PATH="${TMP_DIR}/bin:${PATH}" \
GITHUB_ACTION_PATH="${ROOT_DIR}" \
GITHUB_WORKSPACE="${TMP_DIR}/workspace" \
GITHUB_OUTPUT="${TMP_DIR}/github-output" \
GITHUB_ENV="${TMP_DIR}/github-env" \
RESOLVED_COMMANDS='review test' \
COMPONENT_NAME='homeboy-action' \
bash "${ROOT_DIR}/scripts/core/run-homeboy-commands.sh" >"${TMP_DIR}/run.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 1 ]; then
  printf 'FAIL: failed review test exits 1, got %s\n' "${exit_code}"
  exit 1
fi
if ! grep -q '^results={"review test":"fail"}$' "${TMP_DIR}/github-output"; then
  printf 'FAIL: failed review test is recorded as fail\n'
  exit 1
fi
if ! grep -q 'FAILED (exit code 1)' "${TMP_DIR}/run.log"; then
  printf 'FAIL: failed review test reports its exit code\n'
  exit 1
fi

printf 'PASS: failed review test records a current-run failure\n'

cat > "${TMP_DIR}/bin/homeboy" <<'SH'
#!/usr/bin/env bash
trap '' TERM
(trap '' TERM; while :; do sleep 1; done) &
while :; do sleep 1; done
SH
chmod +x "${TMP_DIR}/bin/homeboy"

set +e
PATH="${TMP_DIR}/bin:${PATH}" \
GITHUB_ACTION_PATH="${ROOT_DIR}" \
GITHUB_WORKSPACE="${TMP_DIR}/workspace" \
GITHUB_OUTPUT="${TMP_DIR}/timeout-output" \
GITHUB_ENV="${TMP_DIR}/timeout-env" \
RESOLVED_COMMANDS='review test' \
COMPONENT_NAME='homeboy-action' \
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=1 \
HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS=1 \
bash "${ROOT_DIR}/scripts/core/run-homeboy-commands.sh" >"${TMP_DIR}/timeout.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 1 ] || ! grep -q '^results={"review test":"timeout"}$' "${TMP_DIR}/timeout-output"; then
  printf 'FAIL: timed out review test is classified for final enforcement\n'
  exit 1
fi
if ! grep -q 'TIMED OUT (exit code 124)' "${TMP_DIR}/timeout.log"; then
  printf 'FAIL: timed out review test does not report its timeout classification\n'
  exit 1
fi
printf 'PASS: timed out review test preserves actionable timeout classification\n'
