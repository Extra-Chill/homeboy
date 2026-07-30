#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

PROJECT_DIR="${TMP_DIR}/project"
BIN_DIR="${TMP_DIR}/bin"
mkdir -p "${PROJECT_DIR}" "${BIN_DIR}"

cat > "${PROJECT_DIR}/dist-workspace.toml" <<'TOML'
[dist]
tap = "Extra-Chill/homebrew-tap"
TOML

cat > "${PROJECT_DIR}/homeboy.rb" <<'RUBY'
class Homeboy < Formula
end
RUBY

cat > "${BIN_DIR}/cargo" <<'SH'
#!/usr/bin/env bash
case "$1" in
  metadata)
    printf '{"workspace_members":["homeboy 1.2.3"],"packages":[{"id":"homeboy 1.2.3","name":"homeboy","version":"1.2.3","publish":null}],"resolve":{"nodes":[{"id":"homeboy 1.2.3","deps":[]}]}}'
    ;;
  info)
    exit 0
    ;;
  *)
    printf 'unexpected cargo command: %s\n' "$*" >&2
    exit 1
    ;;
esac
SH
chmod +x "${BIN_DIR}/cargo"

cat > "${BIN_DIR}/gh" <<'SH'
#!/usr/bin/env bash
if [[ "$*" != "auth token" ]]; then
  exit 1
fi

case "${GH_AUTH_SCENARIO}" in
  fallback)
    printf '%s\n' "${GH_AUTH_TOKEN}"
    ;;
  *)
    exit 1
    ;;
esac
SH
chmod +x "${BIN_DIR}/gh"

cat > "${BIN_DIR}/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "clone" ]]; then
  [[ -n "${GIT_ASKPASS:-}" ]] || exit 1
  [[ "$("${GIT_ASKPASS}" 'Username for https://github.com')" == "x-access-token" ]] || exit 1
  [[ "$("${GIT_ASKPASS}" 'Password for https://github.com')" == "${EXPECTED_TOKEN}" ]] || exit 1
  mkdir -p "${@: -1}"
  printf 'askpass credential matched\n' >> "${EVENT_LOG}"
  exit 0
fi

if [[ "$*" == *' diff --cached --quiet' ]]; then
  exit 0
fi

exit 0
SH
chmod +x "${BIN_DIR}/git"

run_publish() {
  local scenario="$1" expected_token="$2" output
  output="${TMP_DIR}/${scenario}.out"
  shift 2

  : > "${TMP_DIR}/${scenario}.events"
  if ! (
    cd "${PROJECT_DIR}"
    PATH="${BIN_DIR}:${PATH}" \
    EVENT_LOG="${TMP_DIR}/${scenario}.events" \
    EXPECTED_TOKEN="${expected_token}" \
    GH_AUTH_SCENARIO="${scenario}" \
    GH_AUTH_TOKEN="gh-fallback-token" \
    HOMEBOY_HOMEBREW_SKIP_STYLE=true \
    HOMEBOY_SETTINGS_JSON='{"release":{"version":"1.2.3","artifacts":[{"path":"homeboy.rb","type":"homebrew"}]}}' \
    "$@" bash "${SCRIPT_DIR}/publish-crates.sh"
  ) >"${output}" 2>&1; then
    printf '%s publishing unexpectedly failed:\n' "${scenario}" >&2
    cat "${output}" >&2
    exit 1
  fi

  if [[ "$(<"${TMP_DIR}/${scenario}.events")" != "askpass credential matched" ]]; then
    printf '%s did not provide the resolved credential to GIT_ASKPASS\n' "${scenario}" >&2
    exit 1
  fi
}

run_publish explicit-precedence homebrew-token env HOMEBREW_TAP_TOKEN=homebrew-token GH_TOKEN=github-token
run_publish fallback gh-fallback-token env -u HOMEBREW_TAP_TOKEN -u GH_TOKEN

MISSING_OUTPUT="${TMP_DIR}/missing.out"
if (
  cd "${PROJECT_DIR}"
  PATH="${BIN_DIR}:${PATH}" \
  GH_AUTH_SCENARIO=missing \
  HOMEBOY_HOMEBREW_SKIP_STYLE=true \
  HOMEBOY_SETTINGS_JSON='{"release":{"version":"1.2.3","artifacts":[{"path":"homeboy.rb","type":"homebrew"}]}}' \
  env -u HOMEBREW_TAP_TOKEN -u GH_TOKEN bash "${SCRIPT_DIR}/publish-crates.sh"
) >"${MISSING_OUTPUT}" 2>&1; then
  printf 'missing credentials unexpectedly succeeded\n' >&2
  exit 1
fi

if ! grep -Fqx 'Homebrew tap publishing requires HOMEBREW_TAP_TOKEN, GH_TOKEN, or an authenticated gh CLI.' "${MISSING_OUTPUT}"; then
  printf 'missing credentials did not produce the expected diagnostic\n' >&2
  cat "${MISSING_OUTPUT}" >&2
  exit 1
fi

if grep -Fq homebrew-token "${TMP_DIR}"/*.out || grep -Fq github-token "${TMP_DIR}"/*.out || grep -Fq gh-fallback-token "${TMP_DIR}"/*.out; then
  printf 'a Homebrew credential was printed\n' >&2
  exit 1
fi

printf 'publish-crates authentication test passed\n'
