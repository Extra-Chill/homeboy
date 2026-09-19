#!/usr/bin/env bash

# The action installs the shared toolchain; these assert it says where.
#
# phpunit, phpcs, wpcs, phpstan and the WordPress stubs all live inside an
# installed extension. Until the location was published, a `run:` step in a
# consuming workflow could reach none of it — `homeboy: command not found`
# one way, `vendor/bin/phpunit: No such file or directory` the other — so
# every consumer with a custom test or lint step kept a private copy of the
# same tools, pinned independently of the ones CI actually runs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TMP_ROOT}"' EXIT

assert_line() {
  local line="$1" file="$2" label="$3"
  if ! grep -Fxq -- "${line}" "${file}"; then
    printf 'FAIL: %s\nmissing exact line: %s\ngot:\n' "${label}" "${line}"
    cat "${file}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

assert_absent() {
  local needle="$1" file="$2" label="$3"
  if grep -Fq -- "${needle}" "${file}" 2>/dev/null; then
    printf 'FAIL: %s\nunexpectedly present: %s\n' "${label}" "${needle}"
    exit 1
  fi
  printf 'PASS: %s\n' "${label}"
}

# Exercise only the publishing function, with a fake HOME holding an installed
# extension and a fake `homeboy` on PATH.
run_publish() {
  local home="$1" env_file="$2" path_file="$3" ext_input="$4" ext_id="$5" bin_dir="$6"

  (
    set -euo pipefail
    export HOME="${home}"
    export GITHUB_ENV="${env_file}"
    export GITHUB_PATH="${path_file}"
    export EXTENSION_INPUT="${ext_input}"
    export EXTENSION_ID="${ext_id}"
    [ -n "${bin_dir}" ] && export PATH="${bin_dir}:${PATH}"
    # Source only the function definition, not the install flow above it.
    eval "$(sed -n '/^publish_toolchain_location() {/,/^}$/p' "${ROOT_DIR}/scripts/setup/install-extension.sh")"
    publish_toolchain_location
  )
}

FAKE_HOME="${TMP_ROOT}/home"
BIN_DIR="${TMP_ROOT}/bin"
mkdir -p "${FAKE_HOME}/.config/homeboy/extensions/wordpress" "${BIN_DIR}"
printf '#!/usr/bin/env bash\necho homeboy\n' > "${BIN_DIR}/homeboy"
chmod +x "${BIN_DIR}/homeboy"

ENV_FILE="${TMP_ROOT}/github_env"
PATH_FILE="${TMP_ROOT}/github_path"
: > "${ENV_FILE}"
: > "${PATH_FILE}"

run_publish "${FAKE_HOME}" "${ENV_FILE}" "${PATH_FILE}" "" "wordpress" "${BIN_DIR}"

assert_line "HOMEBOY_EXTENSIONS_ROOT=${FAKE_HOME}/.config/homeboy/extensions" "${ENV_FILE}" \
  "the extensions root is published"
assert_line "HOMEBOY_EXTENSION_PATH=${FAKE_HOME}/.config/homeboy/extensions/wordpress" "${ENV_FILE}" \
  "the resolved extension path is published"
assert_line "${BIN_DIR}" "${PATH_FILE}" \
  "the homeboy binary directory is added to PATH"

# An extension that was not installed must not be advertised as a path that
# does not exist — the root is still published so a consumer can compose one.
ENV_FILE2="${TMP_ROOT}/github_env2"
: > "${ENV_FILE2}"
run_publish "${FAKE_HOME}" "${ENV_FILE2}" "${TMP_ROOT}/github_path2" "" "nodejs" "${BIN_DIR}"

assert_line "HOMEBOY_EXTENSIONS_ROOT=${FAKE_HOME}/.config/homeboy/extensions" "${ENV_FILE2}" \
  "the root is published even when the extension is absent"
assert_absent "HOMEBOY_EXTENSION_PATH=" "${ENV_FILE2}" \
  "no extension path is published for an extension that was not installed"

# EXTENSION_INPUT wins over EXTENSION_ID, matching how the install flow above
# selects which extension this invocation is about.
mkdir -p "${FAKE_HOME}/.config/homeboy/extensions/rust"
ENV_FILE3="${TMP_ROOT}/github_env3"
: > "${ENV_FILE3}"
run_publish "${FAKE_HOME}" "${ENV_FILE3}" "${TMP_ROOT}/github_path3" "rust" "wordpress" "${BIN_DIR}"

assert_line "HOMEBOY_EXTENSION_PATH=${FAKE_HOME}/.config/homeboy/extensions/rust" "${ENV_FILE3}" \
  "an explicit extension input selects the published path"

# Outside GitHub Actions there is nothing to publish to; this must not fail.
(
  set -euo pipefail
  export HOME="${FAKE_HOME}"
  unset GITHUB_ENV GITHUB_PATH
  export EXTENSION_ID="wordpress"
  eval "$(sed -n '/^publish_toolchain_location() {/,/^}$/p' "${ROOT_DIR}/scripts/setup/install-extension.sh")"
  publish_toolchain_location
) && printf 'PASS: %s\n' "publishing is a no-op outside GitHub Actions"

printf 'publish toolchain location tests passed\n'
