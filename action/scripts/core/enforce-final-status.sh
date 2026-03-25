#!/usr/bin/env bash

set -euo pipefail

if [ -z "${RESULTS:-}" ] || [ "${RESULTS}" = "{}" ]; then
  # If the PR was merged/closed before commands ran, empty results are expected
  if [ "${PR_ACTIVE:-}" = "false" ]; then
    echo "PR was merged or closed before commands ran — nothing to enforce"
    exit 0
  fi

  # If the only command is "release", empty results are expected (release runs separately)
  COMMANDS="${COMMANDS:-}"
  NON_RELEASE="$(echo "${COMMANDS}" | tr ',' '\n' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | grep -v '^release$' | grep -v '^$' || true)"
  if [ -z "${NON_RELEASE}" ]; then
    echo "Release-only mode — no quality gate commands to enforce"
    exit 0
  fi
  echo "::error::No command results were produced"
  exit 1
fi

if echo "${RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null; then
  echo "::error::One or more Homeboy commands failed"
  exit 1
fi

echo "All Homeboy commands passed"
