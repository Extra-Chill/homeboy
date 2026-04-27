#!/usr/bin/env bash

set -euo pipefail

OPERATIONS_RESULTS="${OPERATIONS_RESULTS:-}"
HAS_QUALITY_COMMANDS=true
HAS_OPERATIONS_COMMANDS=true

if [ -z "${RESULTS:-}" ] || [ "${RESULTS}" = "{}" ]; then
  HAS_QUALITY_COMMANDS=false

  # If the PR was merged/closed before commands ran, empty results are expected
  if [ "${PR_ACTIVE:-}" = "false" ]; then
    echo "PR was merged or closed before commands ran — nothing to enforce"
    exit 0
  fi

  # If there are no quality commands, empty quality results are expected.
  COMMANDS="${COMMANDS:-}"
  if [ -z "${COMMANDS}" ] && [ -z "${OPERATIONS_RESULTS}" ]; then
    echo "No quality gate commands to enforce"
    exit 0
  fi

  # If we have operations results but no quality results, that's fine
  if [ -z "${COMMANDS}" ] && [ -n "${OPERATIONS_RESULTS}" ]; then
    HAS_QUALITY_COMMANDS=false
  elif [ -z "${OPERATIONS_RESULTS}" ]; then
    echo "::error::No command results were produced"
    exit 1
  fi
fi

FAILED=false

# Check quality command results
if [ "${HAS_QUALITY_COMMANDS}" = true ]; then
  if echo "${RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null; then
    echo "::error::One or more quality commands failed"
    FAILED=true
  fi
fi

# Check operations command results
if [ -n "${OPERATIONS_RESULTS}" ] && [ "${OPERATIONS_RESULTS}" != "{}" ]; then
  if echo "${OPERATIONS_RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null; then
    echo "::error::One or more operations commands (fleet/deploy) failed"
    FAILED=true
  fi
fi

if [ "${FAILED}" = true ]; then
  exit 1
fi

echo "All Homeboy commands passed"
