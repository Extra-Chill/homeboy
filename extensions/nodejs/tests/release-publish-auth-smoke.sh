#!/usr/bin/env bash
# publish.sh hands a runner's NPM_TOKEN to npm without exposing it, and leaves
# tokenless (trusted publishing / OIDC) releases to npm with provenance.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PUBLISH_SCRIPT="$(cd "${SCRIPT_DIR}/.." && pwd)/scripts/release/publish.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT
SECRET='npm-secret-must-not-be-printed'

# A fake npm records how publish was authenticated, as npm itself would read it.
mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/pkg"
cat >"${TMP_DIR}/bin/npm" <<'NPM'
#!/usr/bin/env bash
case "${1:-}" in
  view) exit 1 ;;
  publish)
    {
      printf 'args=%s\n' "$*"
      printf 'node_auth_token=%s\n' "${NODE_AUTH_TOKEN:-}"
      if [[ -n "${NPM_CONFIG_USERCONFIG:-}" && -f "${NPM_CONFIG_USERCONFIG}" ]]; then
        printf 'userconfig=%s\n' "$(cat "${NPM_CONFIG_USERCONFIG}")"
        printf 'userconfig_path=%s\n' "${NPM_CONFIG_USERCONFIG}"
      fi
    } >"${NPM_RECORD}"
    ;;
esac
NPM
chmod +x "${TMP_DIR}/bin/npm"
printf '{"name":"fixture-pkg","version":"1.2.3"}\n' >"${TMP_DIR}/pkg/package.json"

fail() { printf 'FAIL: %s\n' "$1"; exit 1; }
pass() { printf 'PASS: %s\n' "$1"; }
run_publish() {
  (cd "${TMP_DIR}/pkg" && PATH="${TMP_DIR}/bin:${PATH}" NPM_RECORD="${TMP_DIR}/record" \
    HOMEBOY_SETTINGS_JSON='{"config":{"registry":"https://registry.example.test/"}}' "$@" bash "${PUBLISH_SCRIPT}" 2>&1)
}

output="$(run_publish env NPM_TOKEN="${SECRET}")"
record="$(cat "${TMP_DIR}/record")"
grep -Fq "node_auth_token=${SECRET}" <<<"${record}" || fail 'token reaches npm through NODE_AUTH_TOKEN'
pass 'token reaches npm through NODE_AUTH_TOKEN'
grep -Fq 'userconfig=//registry.example.test/:_authToken=${NODE_AUTH_TOKEN}' <<<"${record}" || fail 'userconfig references the token by variable for the configured registry'
pass 'userconfig references the token by variable for the configured registry'
grep -Fq "${SECRET}" <<<"$(grep '^userconfig=' <<<"${record}")" && fail 'token value is never written to the userconfig'
pass 'token value is never written to the userconfig'
grep -Fq "${SECRET}" <<<"${output}" && fail 'token value is never printed'
pass 'token value is never printed'
userconfig_path="$(sed -n 's/^userconfig_path=//p' <<<"${record}")"
[[ -e "${userconfig_path}" ]] && fail 'temporary userconfig is removed after publish'
pass 'temporary userconfig is removed after publish'
grep -Fq -- '--provenance' <<<"${record}" && fail 'token publishing does not request provenance'
pass 'token publishing does not request provenance'

run_publish env NPM_CONFIG_PROVENANCE=true >/dev/null
record="$(cat "${TMP_DIR}/record")"
grep -Fq -- '--provenance' <<<"${record}" || fail 'tokenless publishing requests provenance'
pass 'tokenless publishing requests provenance'
grep -q '^node_auth_token=$' <<<"${record}" || fail 'tokenless publishing sets no token'
pass 'tokenless publishing sets no token'
