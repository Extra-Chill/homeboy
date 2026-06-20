#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

assert_contains() {
  local needle="$1"
  local haystack="$2"
  local label="$3"

  if ! grep -Fq -- "${needle}" "${haystack}"; then
    printf 'FAIL: %s\nmissing: %s\n' "${label}" "${needle}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

mkdir -p "${TMP_DIR}/bin" "${TMP_DIR}/home/.config/homeboy/extensions/wordpress"
cat > "${TMP_DIR}/bin/homeboy" <<'STUB'
#!/usr/bin/env bash
if [ "$1" = "--version" ]; then
  printf 'homeboy 9.9.9\n'
fi
STUB
chmod +x "${TMP_DIR}/bin/homeboy"

printf 'abc1234\n' > "${TMP_DIR}/home/.config/homeboy/extensions/wordpress/.source-revision"

export PATH="${TMP_DIR}/bin:${PATH}"
export HOME="${TMP_DIR}/home"
export PORTABLE_EXTENSION="wordpress"
export EXTENSION_SOURCE="https://github.com/Extra-Chill/homeboy-extensions"
export ACTION_REF="v2"
export ACTION_REPOSITORY="Extra-Chill/homeboy-action"
export GITHUB_ENV="${TMP_DIR}/github-env"
export GITHUB_OUTPUT="${TMP_DIR}/github-output"

bash "${ROOT_DIR}/scripts/setup/capture-tooling-metadata.sh" > "${TMP_DIR}/metadata.log"

assert_contains "HOMEBOY_EXTENSION_REVISION=abc1234" "${GITHUB_ENV}" "source revision file is exported"
assert_contains "- Extension revision: abc1234" "${TMP_DIR}/metadata.log" "source revision file is logged"

# Tooling identity output for failed-SHA marker keying (homeboy-action#257).
# The raw version string "homeboy 9.9.9" contains a space, which is hostile to
# GitHub cache keys; the sanitizer must collapse it to a '-'. A dev build would
# add a '+sha' suffix — also sanitized — so any tooling change yields a fresh,
# cache-key-safe identity token.
assert_contains "homeboy-version=homeboy 9.9.9" "${GITHUB_OUTPUT}" "resolved homeboy version is exposed verbatim as a step output"
assert_contains "tooling-identity=homeboy-9.9.9-abc1234" "${GITHUB_OUTPUT}" "tooling identity combines sanitized version + extension revision into a cache-key-safe token"

echo "All tooling metadata checks passed."
