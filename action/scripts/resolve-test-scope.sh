#!/usr/bin/env bash

set -euo pipefail

REQUESTED_SCOPE="${TEST_SCOPE:-full}"

if [ "${REQUESTED_SCOPE}" != "changed" ]; then
  echo "effective-test-scope=${REQUESTED_SCOPE}" >> "${GITHUB_OUTPUT}"
  exit 0
fi

if [ -z "${HOMEBOY_CHANGED_SINCE:-}" ]; then
  echo "::warning::test-scope=changed requested but no PR base ref was detected; using full test scope"
  echo "effective-test-scope=full" >> "${GITHUB_OUTPUT}"
  echo "HOMEBOY_TEST_SCOPE_EFFECTIVE=full" >> "${GITHUB_ENV}"
  exit 0
fi

if homeboy test --help 2>/dev/null | grep -q -- '--changed-since'; then
  echo "effective-test-scope=changed" >> "${GITHUB_OUTPUT}"
  echo "HOMEBOY_TEST_SCOPE_EFFECTIVE=changed" >> "${GITHUB_ENV}"
  exit 0
fi

echo "::warning::homeboy test does not support --changed-since yet; falling back to full test scope"
echo "effective-test-scope=full" >> "${GITHUB_OUTPUT}"
echo "HOMEBOY_TEST_SCOPE_EFFECTIVE=full" >> "${GITHUB_ENV}"
