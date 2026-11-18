#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

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

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin"
cat > "${TMP_DIR}/bin/homeboy" <<'STUB'
#!/usr/bin/env bash
if [ "$*" = "extension install-for-component --help" ]; then
  exit "${HOMEBOY_INSTALL_FOR_COMPONENT_HELP_STATUS:-0}"
fi
printf '%s\n' "$*" >> "${HOMEBOY_CALL_LOG}"
# Simulate a source that cannot serve the install (e.g. a ref missing there).
if [ -n "${HOMEBOY_FAIL_SOURCE:-}" ] && [[ " $* " == *" ${HOMEBOY_FAIL_SOURCE} "* ]]; then
  exit 1
fi
# With HOMEBOY_STUB_REVISION set, materialize the tree the way Homeboy does: a
# manifest plus the resolved revision Homeboy records in `.source-revision`.
if [ -n "${HOMEBOY_STUB_REVISION:-}" ]; then
  extensions_dir="${HOMEBOY_CONFIG_ROOT}/extensions"
  if [ "$1 $2" = "extension uninstall" ]; then
    rm -rf "${extensions_dir:?}/$3"
  elif [ "$1 $2" = "extension install" ]; then
    id="$5"
    mkdir -p "${extensions_dir}/${id}"
    printf '{"id":"%s"}\n' "${id}" > "${extensions_dir}/${id}/${id}.json"
    printf '%s' "${HOMEBOY_STUB_REVISION}" > "${extensions_dir}/${id}/.source-revision"
  fi
fi
STUB
chmod +x "${TMP_DIR}/bin/homeboy"

export PATH="${TMP_DIR}/bin:${PATH}"
export EXTENSION_SOURCE="/extensions"
export EXTENSION_REF=""
export EXTENSION_ID="wordpress"
export COMPONENT_DIR="${TMP_DIR}/component"
export HOMEBOY_CALL_LOG="${TMP_DIR}/calls.log"
export HOMEBOY_CONFIG_ROOT="${TMP_DIR}/config"

mkdir -p "${COMPONENT_DIR}"
cat > "${COMPONENT_DIR}/homeboy.json" <<'JSON'
{
  "id": "demo",
  "extensions": {
    "wordpress": {},
    "nodejs": {}
  }
}
JSON

EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source /extensions' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "all configured extensions refresh cached copy before install-for-component"

: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_INSTALL_FOR_COMPONENT_HELP_STATUS="1" EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall wordpress\nextension install /extensions --id wordpress' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "older homeboy refreshes cached copy before fallback install"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_INPUT="nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "explicit extension input refreshes cached copy before install"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_REF="aeb1bab" EXTENSION_INPUT="nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aeb1bab' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "explicit extension input installs the requested revision"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_REF="aeb1bab" EXTENSION_INPUT="" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aeb1bab\nextension uninstall wordpress\nextension install /extensions --id wordpress --ref aeb1bab' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "configured extensions install the requested revision"

: > "${HOMEBOY_CALL_LOG}"
if EXTENSION_INPUT="wordpress,nodejs" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>"${TMP_DIR}/comma.err"; then
  echo "FAIL: comma-separated extension input should fail"
  exit 1
fi
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "comma-separated input does not invoke homeboy"

MONO="https://github.com/Extra-Chill/homeboy"
LEGACY="https://github.com/Extra-Chill/homeboy-extensions"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_SOURCE="${MONO}" EXTENSION_REF="wordpress-v3.53.12" EXTENSION_INPUT="wordpress" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null
assert_equals \
  $'extension uninstall wordpress\nextension install '"${MONO}"$' --id wordpress --ref wordpress-v3.53.12' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "monorepo source installs without touching the frozen repository"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_SOURCE="${MONO}" EXTENSION_REF="dd63b7be" EXTENSION_INPUT="" HOMEBOY_FAIL_SOURCE="${MONO}" \
  bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >"${TMP_DIR}/fallback.out" 2>&1
assert_equals \
  $'extension uninstall nodejs\nextension install '"${MONO}"$' --id nodejs --ref dd63b7be\nextension uninstall nodejs\nextension install '"${LEGACY}"$' --id nodejs --ref dd63b7be\nextension uninstall wordpress\nextension install '"${LEGACY}"$' --id wordpress --ref dd63b7be' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "a failed monorepo install stops at the first failure and retries every extension from the frozen repository"
assert_equals "1" "$(grep -c '^::warning::.*retrying from the frozen' "${TMP_DIR}/fallback.out")" "fallback emits one warning"

: > "${HOMEBOY_CALL_LOG}"
EXTENSION_SOURCE="${MONO}.git" EXTENSION_INPUT="" HOMEBOY_FAIL_SOURCE="${MONO}.git" \
  bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>&1
assert_equals \
  $'extension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source '"${MONO}"$'.git\nextension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source '"${LEGACY}" \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "install-for-component falls back too, and a .git suffix still counts as the monorepo"

: > "${HOMEBOY_CALL_LOG}"
if EXTENSION_SOURCE="/custom" EXTENSION_INPUT="wordpress" HOMEBOY_FAIL_SOURCE="/custom" \
  bash "${ROOT_DIR}/scripts/setup/install-extension.sh" >/dev/null 2>&1; then
  echo "FAIL: a failing custom source should fail without a fallback"
  exit 1
fi
assert_equals \
  $'extension uninstall wordpress\nextension install /custom --id wordpress' \
  "$(cat "${HOMEBOY_CALL_LOG}")" \
  "a custom extension-source never falls back"

# ── Cached install reuse (Extra-Chill/homeboy#18462) ──

EXT_DIR="${HOMEBOY_CONFIG_ROOT}/extensions"
REV_A="$(printf 'a%.0s' {1..40})"
REV_B="$(printf 'b%.0s' {1..40})"

reset_store() {
  rm -rf "${HOMEBOY_CONFIG_ROOT}"
  mkdir -p "${EXT_DIR}"
  : > "${HOMEBOY_CALL_LOG}"
}

run_install() {
  EXTENSION_INPUT="${EXTENSION_INPUT:-}" bash "${ROOT_DIR}/scripts/setup/install-extension.sh" > "${TMP_DIR}/run.out" 2>&1
}

INSTALL_BOTH_A=$'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aaaaaaa\nextension uninstall wordpress\nextension install /extensions --id wordpress --ref aaaaaaa'

# Matching immutable SHA ref: the second run reuses every configured extension.
reset_store
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
assert_equals "${INSTALL_BOTH_A}" "$(cat "${HOMEBOY_CALL_LOG}")" "first pinned run installs every configured extension"
assert_equals "source=/extensions" "$(sed -n 1p "${EXT_DIR}/.wordpress.homeboy-action-install")" "marker records the normalized source"
assert_equals "ref=aaaaaaa" "$(sed -n 2p "${EXT_DIR}/.wordpress.homeboy-action-install")" "marker records the requested ref"
assert_equals "commit=${REV_A}" "$(sed -n 3p "${EXT_DIR}/.wordpress.homeboy-action-install")" "marker records the resolved commit"

: > "${HOMEBOY_CALL_LOG}"
: > "${TMP_DIR}/github-env"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" GITHUB_ENV="${TMP_DIR}/github-env" run_install
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "matching immutable SHA ref skips uninstall and install"
assert_equals "2" "$(grep -c "^Reusing cached extension '.*' from /extensions at aaaaaaa (commit ${REV_A}); skipping reinstall.$" "${TMP_DIR}/run.out")" "every reused extension logs the reuse"
assert_equals "1" "$(grep -c '^HOMEBOY_EXTENSIONS_ROOT=' "${TMP_DIR}/github-env")" "toolchain location is still published after reuse"

# The full SHA and a trailing .git / slash on the source also match.
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_SOURCE="/extensions.git/" EXTENSION_REF="aaaaaaa" run_install
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "source normalization strips a trailing slash and .git"

# A different ref reinstalls.
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_B}" EXTENSION_REF="bbbbbbb" run_install
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref bbbbbbb\nextension uninstall wordpress\nextension install /extensions --id wordpress --ref bbbbbbb' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "a changed ref reinstalls"

# A different source reinstalls.
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_B}" EXTENSION_SOURCE="/other" EXTENSION_REF="bbbbbbb" run_install
assert_equals "4" "$(wc -l < "${HOMEBOY_CALL_LOG}" | tr -d ' ')" "a changed source reinstalls"

# A missing marker reinstalls only that extension.
reset_store
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
rm -f "${EXT_DIR}/.wordpress.homeboy-action-install"
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
assert_equals \
  $'extension uninstall wordpress\nextension install /extensions --id wordpress --ref aaaaaaa' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "a missing marker reinstalls that extension only"

# A missing manifest (partial tree) reinstalls.
rm -f "${EXT_DIR}/nodejs/nodejs.json"
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aaaaaaa' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "a missing manifest reinstalls"

# A tree whose recorded revision no longer matches the marker reinstalls.
printf '%s' "${REV_B}" > "${EXT_DIR}/nodejs/.source-revision"
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
assert_equals \
  $'extension uninstall nodejs\nextension install /extensions --id nodejs --ref aaaaaaa' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "a revision that drifted from the marker reinstalls"

# An explicit extension input with an immutable ref reuses too.
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" EXTENSION_INPUT="nodejs" run_install
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "explicit extension input with an immutable ref reuses"

# Without a ref, the cached copy is always refreshed and its marker dropped.
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="" EXTENSION_INPUT="nodejs" run_install
assert_equals $'extension uninstall nodejs\nextension install /extensions --id nodejs' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "explicit extension input without a ref refreshes"
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="" run_install
assert_equals \
  $'extension uninstall nodejs\nextension uninstall wordpress\nextension install-for-component --path '"${COMPONENT_DIR}"$' --source /extensions' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "install-for-component keeps refreshing"
if [ -e "${EXT_DIR}/.wordpress.homeboy-action-install" ]; then
  echo "FAIL: a refresh must remove the reuse marker"; exit 1
fi
echo "PASS: a refresh removes the reuse marker"

# Tags and branches resolve against a real (local) git source.
SRC_REPO="${TMP_DIR}/src-repo"
git init -q "${SRC_REPO}"
git -C "${SRC_REPO}" -c user.name=t -c user.email=t@t commit -q --allow-empty -m one
git -C "${SRC_REPO}" -c user.name=t -c user.email=t@t tag -a v1.0.0 -m v1.0.0
git -C "${SRC_REPO}" tag light-1.0.0
git -C "${SRC_REPO}" branch -q release
TAG_COMMIT="$(git -C "${SRC_REPO}" rev-parse HEAD)"

reset_store
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="v1.0.0" run_install
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="v1.0.0" run_install
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "an annotated tag resolving to the recorded commit reuses"

reset_store
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="light-1.0.0" run_install
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="light-1.0.0" run_install
assert_equals "" "$(cat "${HOMEBOY_CALL_LOG}")" "a lightweight tag resolving to the recorded commit reuses"

# Branch refs are mutable: always reinstall.
reset_store
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="release" run_install
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="release" run_install
assert_equals \
  $'extension uninstall nodejs\nextension install '"${SRC_REPO}"$' --id nodejs --ref release\nextension uninstall wordpress\nextension install '"${SRC_REPO}"$' --id wordpress --ref release' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "a branch ref reinstalls"

# A moved tag reinstalls.
reset_store
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="v1.0.0" run_install
git -C "${SRC_REPO}" -c user.name=t -c user.email=t@t commit -q --allow-empty -m two
git -C "${SRC_REPO}" -c user.name=t -c user.email=t@t tag -f -a v1.0.0 -m moved >/dev/null
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="v1.0.0" run_install
assert_equals "4" "$(wc -l < "${HOMEBOY_CALL_LOG}" | tr -d ' ')" "a tag that moved off the recorded commit reinstalls"

# A tag that is also a branch name is ambiguous: reinstall.
git -C "${SRC_REPO}" tag ambiguous
git -C "${SRC_REPO}" branch -q ambiguous
AMBIGUOUS_COMMIT="$(git -C "${SRC_REPO}" rev-parse HEAD)"
reset_store
HOMEBOY_STUB_REVISION="${AMBIGUOUS_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="ambiguous" run_install
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${AMBIGUOUS_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="ambiguous" run_install
assert_equals "4" "$(wc -l < "${HOMEBOY_CALL_LOG}" | tr -d ' ')" "a ref that names both a tag and a branch reinstalls"

# A reuse check that errors falls back to a fresh install, never a failure.
reset_store
HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="light-1.0.0" run_install
mkdir -p "${TMP_DIR}/broken-git"
printf '#!/usr/bin/env bash\necho "fatal: simulated ls-remote failure" >&2\nexit 128\n' > "${TMP_DIR}/broken-git/git"
chmod +x "${TMP_DIR}/broken-git/git"
: > "${HOMEBOY_CALL_LOG}"
PATH="${TMP_DIR}/broken-git:${PATH}" HOMEBOY_STUB_REVISION="${TAG_COMMIT}" EXTENSION_SOURCE="${SRC_REPO}" EXTENSION_REF="light-1.0.0" run_install
assert_equals "4" "$(wc -l < "${HOMEBOY_CALL_LOG}" | tr -d ' ')" "a failing tag lookup falls back to a fresh install"

reset_store
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
printf '\0\377garbage' > "${EXT_DIR}/.nodejs.homeboy-action-install"
rm -f "${EXT_DIR}/.wordpress.homeboy-action-install"
mkdir "${EXT_DIR}/.wordpress.homeboy-action-install"
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_REF="aaaaaaa" run_install
assert_equals "${INSTALL_BOTH_A}" "$(cat "${HOMEBOY_CALL_LOG}")" "a corrupt or unreadable marker falls back to a fresh install"

# The legacy-source fallback never reuses: every extension is reinstalled.
reset_store
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_SOURCE="${MONO}" EXTENSION_REF="aaaaaaa" run_install
rm -f "${EXT_DIR}/.wordpress.homeboy-action-install"
: > "${HOMEBOY_CALL_LOG}"
HOMEBOY_STUB_REVISION="${REV_A}" EXTENSION_SOURCE="${MONO}" EXTENSION_REF="aaaaaaa" HOMEBOY_FAIL_SOURCE="${MONO}" run_install
assert_equals \
  $'extension uninstall wordpress\nextension install '"${MONO}"$' --id wordpress --ref aaaaaaa\nextension uninstall nodejs\nextension install '"${LEGACY}"$' --id nodejs --ref aaaaaaa\nextension uninstall wordpress\nextension install '"${LEGACY}"$' --id wordpress --ref aaaaaaa' \
  "$(cat "${HOMEBOY_CALL_LOG}")" "the legacy-source fallback reinstalls every extension"

echo "All install-extension checks passed."
