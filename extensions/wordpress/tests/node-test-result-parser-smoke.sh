#!/usr/bin/env bash
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOSITORY_ROOT="$(cd "${WORDPRESS_ROOT}/.." && pwd)"
CORE_RUNTIME_DIR="${HOMEBOY_CORE_DIR:-$(cd "${REPOSITORY_ROOT}/.." && pwd)/homeboy}/crates/homeboy-extension/src/runtime"
FIXTURE="${WORDPRESS_ROOT}/tests/fixtures/node-test-results/nested-tap-package-summary.txt"
RESULTS="$(mktemp "${TMPDIR:-/tmp}/homeboy-node-test-results.XXXXXX")"
trap 'rm -f "$RESULTS"' EXIT

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${CORE_RUNTIME_DIR}/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="$RESULTS" \
    bash "${WORDPRESS_ROOT}/scripts/test/parse-test-results.sh" "$FIXTURE" wp-codebox-json node-test

node - "$RESULTS" <<'JS'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (
  Object.keys(result).sort().join(',') !== 'failed,passed,skipped,total' ||
  !Object.values(result).every(Number.isInteger) ||
  result.total !== 50 || result.passed !== 50 || result.failed !== 0 || result.skipped !== 0
) {
  throw new Error(`Unexpected parsed result: ${JSON.stringify(result)}`);
}
JS

printf '%s\n' 'node test result parser smoke passed'
