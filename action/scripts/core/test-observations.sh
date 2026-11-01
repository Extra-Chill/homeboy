#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

fake_bin="${TMP_DIR}/bin"
mkdir -p "${fake_bin}"

cat > "${fake_bin}/homeboy" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${HOMEBOY_IMPORT_CALL_LOG}"
if [ "$1 $2" = "runs export" ]; then
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--output" ]; then mkdir -p "$2" && touch "$2/manifest.json"; fi
    shift
  done
fi
FAKE
chmod +x "${fake_bin}/homeboy"

export PATH="${fake_bin}:${PATH}"
export GITHUB_ACTION_PATH="${ROOT}"
export GITHUB_WORKSPACE="${TMP_DIR}/workspace"
export RUNNER_TEMP="${TMP_DIR}/runner-temp"
export GITHUB_ENV="${TMP_DIR}/github-env"
export HOMEBOY_IMPORT_CALL_LOG="${TMP_DIR}/homeboy.log"
mkdir -p "${GITHUB_WORKSPACE}" "${RUNNER_TEMP}"
: > "${GITHUB_ENV}"

mkdir -p "${RUNNER_TEMP}/homeboy-observations-import/job-a"
mkdir -p "${RUNNER_TEMP}/homeboy-observations-import/job-b/nested"
touch "${RUNNER_TEMP}/homeboy-observations-import/job-a/manifest.json"
touch "${RUNNER_TEMP}/homeboy-observations-import/job-b/nested/manifest.json"

bash "${ROOT}/scripts/core/import-observations.sh" >/dev/null
bash "${ROOT}/scripts/core/export-observations.sh" >/dev/null

fail() { printf 'FAIL: %s\n' "$1"; exit 1; }
grep -q "runs import ${RUNNER_TEMP}/homeboy-observations-import/job-a" "${HOMEBOY_IMPORT_CALL_LOG}" || fail 'missing import call for first bundle'
grep -q "runs import ${RUNNER_TEMP}/homeboy-observations-import/job-b/nested" "${HOMEBOY_IMPORT_CALL_LOG}" || fail 'missing import call for nested bundle'
[ -f "${RUNNER_TEMP}/homeboy-observations/manifest.json" ] || fail 'export did not write to RUNNER_TEMP'
grep -qx "HOMEBOY_OBSERVATIONS_DIR=${RUNNER_TEMP}/homeboy-observations" "${GITHUB_ENV}" || fail 'export did not publish its directory'
# Baseline reruns and review tests refuse a dirty checkout.
[ -z "$(ls -A "${GITHUB_WORKSPACE}")" ] || fail "observations dirtied the checkout: $(ls -A "${GITHUB_WORKSPACE}")"

printf 'All observation checks passed.\n'
