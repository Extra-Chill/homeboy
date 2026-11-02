#!/usr/bin/env bash
# Private workspaces skip npm publishing and publish validation, not releases.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELEASE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)/scripts/release"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/pkg"
cat >"${TMP_DIR}/bin/npm" <<'NPM'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${NPM_RECORD}"
# Any npm invocation for a private fixture is an error.
[[ "${EXPECT_NO_NPM}" != true ]] || exit 99
case "${1:-}" in
  view) exit 1 ;;
  publish|run|pack) ;;
  *) exit 64 ;;
esac
NPM
chmod +x "${TMP_DIR}/bin/npm"

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
run_step() {
  : >"${TMP_DIR}/record"
  (cd "${TMP_DIR}/pkg" && env -u HOMEBOY_SETTINGS_JSON -u NPM_TOKEN \
    PATH="${TMP_DIR}/bin:${PATH}" NPM_RECORD="${TMP_DIR}/record" \
    EXPECT_NO_NPM="${expect_no_npm}" bash "${RELEASE_DIR}/$1.sh" \
    >"${TMP_DIR}/stdout" 2>"${TMP_DIR}/stderr")
}

for scripts in '{}' '{"validate":"exit 99"}'; do
  printf '{"name":"private-fixture","version":"1.2.3","private":true,"scripts":%s}\n' \
    "${scripts}" >"${TMP_DIR}/pkg/package.json"
  expect_no_npm=true
  for step in publish validate; do
    run_step "${step}" || fail "private ${step} must succeed"
    [[ ! -s "${TMP_DIR}/record" ]] || fail "private ${step} invoked npm"
    [[ ! -s "${TMP_DIR}/stdout" ]] || fail "private ${step} logged to stdout"
    if [[ "${step}" == publish ]]; then
      grep -Fxq 'Skipping npm publish: private-fixture is private' "${TMP_DIR}/stderr" || fail 'missing private publish message'
    else
      grep -Fxq 'Skipping npm publish validation: private-fixture is private' "${TMP_DIR}/stderr" || fail 'missing private validation message'
    fi
  done
done

# Only boolean true is private; missing, false, and string values still publish.
expect_no_npm=false
for private_field in '' ',"private":false' ',"private":"true"'; do
  printf '{"name":"public-fixture","version":"1.2.3"%s}\n' "${private_field}" >"${TMP_DIR}/pkg/package.json"
  run_step publish || fail 'public publish must succeed'
  grep -q '^publish ' "${TMP_DIR}/record" || fail 'public package did not publish'
  run_step validate || fail 'public pack validation must succeed'
  grep -Fxq 'pack --dry-run' "${TMP_DIR}/record" || fail 'public pack validation skipped'
done

printf '{"name":"public-fixture","version":"1.2.3","scripts":{"validate":"custom-validation"}}\n' >"${TMP_DIR}/pkg/package.json"
run_step validate || fail 'public custom validation must succeed'
grep -Fxq 'run validate' "${TMP_DIR}/record" || fail 'public custom validation skipped'

printf 'PASS: private packages skip publishing and validation; public packages still run both\n'
