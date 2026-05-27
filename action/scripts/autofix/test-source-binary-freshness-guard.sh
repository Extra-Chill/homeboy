#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

source "${ROOT}/scripts/core/lib.sh"

assert_guard() {
  local expected="$1"
  local binary_source="$2"
  local built_head_sha="$3"
  local label="$4"

  set +e
  BINARY_SOURCE="${binary_source}" HOMEBOY_CLI_HEAD_SHA="${built_head_sha}" should_enforce_source_binary_freshness
  local actual="$?"
  set -e

  if [ "${actual}" != "${expected}" ]; then
    printf 'FAIL: %s\nexpected: %s\nactual: %s\n' "${label}" "${expected}" "${actual}"
    exit 1
  fi

  printf 'PASS: %s\n' "${label}"
}

assert_guard 0 "source" "abc123" "source builds enforce freshness"
assert_guard 1 "source" "" "source builds without metadata skip freshness guard"
assert_guard 1 "release" "abc123" "release binaries skip freshness guard"
assert_guard 1 "prebuilt" "abc123" "prebuilt binaries skip freshness guard"
assert_guard 1 "fallback" "abc123" "fallback binaries skip freshness guard"
assert_guard 1 "" "abc123" "unknown binary source skips freshness guard"
