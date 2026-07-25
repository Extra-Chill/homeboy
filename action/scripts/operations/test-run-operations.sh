#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin"
cat > "${TMP_DIR}/bin/homeboy" <<'SH'
#!/usr/bin/env bash

trap '' TERM
(trap '' TERM; while :; do sleep 1; done) &
printf 'operations output retained\n'
while :; do sleep 1; done
SH
chmod +x "${TMP_DIR}/bin/homeboy"

set +e
PATH="${TMP_DIR}/bin:${PATH}" \
GITHUB_ACTION_PATH="${ROOT_DIR}" \
GITHUB_OUTPUT="${TMP_DIR}/github-output" \
OPERATIONS_COMMANDS='fleet status fixture' \
HOMEBOY_ACTION_EXECUTION_TIMEOUT_SECONDS=1 \
HOMEBOY_ACTION_CLEANUP_TIMEOUT_SECONDS=1 \
bash "${ROOT_DIR}/scripts/operations/run-operations.sh" >"${TMP_DIR}/run.log" 2>&1
exit_code=$?
set -e

if [ "${exit_code}" -ne 1 ]; then
  printf 'FAIL: timed out operation exits 1, got %s\n' "${exit_code}"
  exit 1
fi
if ! grep -q '^results={"fleet status fixture":"timeout"}$' "${TMP_DIR}/github-output"; then
  printf 'FAIL: timed out operation is not classified for final enforcement\n'
  exit 1
fi
if ! grep -q 'operations homeboy fleet status fixture exceeded its 1s execution timeout' "${TMP_DIR}/run.log"; then
  printf 'FAIL: timed out operation lacks liveness timeout evidence\n'
  exit 1
fi
if ! grep -q 'operations output retained' "${TMP_DIR}/run.log"; then
  printf 'FAIL: timed out operation did not preserve its command log\n'
  exit 1
fi
printf 'PASS: operations command has bounded liveness, process-group cleanup, and timeout classification\n'
