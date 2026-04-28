#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
export GITHUB_ACTION_PATH="${ROOT}"

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" != *"${needle}"* ]]; then
    printf 'FAIL: %s\nmissing: %s\nbody:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"

  if [[ "${haystack}" == *"${needle}"* ]]; then
    printf 'FAIL: %s\nunexpected: %s\nbody:\n%s\n' "${label}" "${needle}" "${haystack}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

export HOMEBOY_CLI_VERSION="homeboy 1.2.3"
export HOMEBOY_EXTENSION_ID="wordpress"
export HOMEBOY_EXTENSION_SOURCE="github"
export HOMEBOY_EXTENSION_REVISION="abc1234"
export HOMEBOY_ACTION_REPOSITORY="Extra-Chill/homeboy-action"
export HOMEBOY_ACTION_REF="feature/footer-test"

source "${ROOT}/scripts/pr/comment/sections.sh"

tooling_section="$(build_tooling_section)"

assert_contains "${tooling_section}" '- Homeboy CLI: `homeboy 1.2.3`' "tooling section includes CLI version"
assert_contains "${tooling_section}" '- Action: `Extra-Chill/homeboy-action@feature/footer-test`' "tooling section renders actual action ref"
assert_not_contains "${tooling_section}" 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1' "tooling section does not hardcode v1 footer"
assert_not_contains "${tooling_section}" 'Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v2' "tooling section does not hardcode v2 footer"
assert_not_contains "${tooling_section}" '---' "tooling section has no redundant footer separator"

printf 'All tooling section checks passed.\n'
