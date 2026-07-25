#!/usr/bin/env bash

set -euo pipefail

OPERATIONS_RESULTS="${OPERATIONS_RESULTS:-}"
HAS_QUALITY_COMMANDS=true
HAS_OPERATIONS_COMMANDS=true

validate_results_json() {
  local label="$1"
  local json="$2"

  if [ -z "${json}" ]; then
    return 0
  fi

  if ! printf '%s\n' "${json}" | jq -e 'type == "object"' > /dev/null 2>&1; then
    echo "::error::${label} results are not valid JSON object data"
    return 1
  fi
}

if [ "${PR_ACTIVE:-}" = "false" ]; then
  echo "PR was merged or closed before final enforcement — ignoring stale command results"
  exit 0
fi

validate_results_json "Quality command" "${RESULTS:-}"
validate_results_json "Operations command" "${OPERATIONS_RESULTS}"

if [ -z "${RESULTS:-}" ] || [ "${RESULTS}" = "{}" ]; then
  HAS_QUALITY_COMMANDS=false

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
  if printf '%s\n' "${RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null; then
    echo "::error::One or more quality commands failed"
    FAILED=true
  fi
  if printf '%s\n' "${RESULTS}" | jq -e 'to_entries | any(.value == "timeout")' > /dev/null; then
    echo "::error::One or more quality commands timed out; inspect the retained command logs above."
    FAILED=true
  fi
  if printf '%s\n' "${RESULTS}" | jq -e 'to_entries | any(.value == "baseline_red")' > /dev/null; then
    echo "::warning::One or more quality commands were inconclusive because the baseline is already red"
  fi
  if printf '%s\n' "${RESULTS}" | jq -e 'to_entries | any(.value == "inconclusive")' > /dev/null; then
    echo "::warning::One or more quality commands were inconclusive; preserving evidence without failing the PR"
  fi
fi

# Check operations command results
if [ -n "${OPERATIONS_RESULTS}" ] && [ "${OPERATIONS_RESULTS}" != "{}" ]; then
  if printf '%s\n' "${OPERATIONS_RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null; then
    echo "::error::One or more operations commands (fleet/deploy) failed"
    FAILED=true
  fi
  if printf '%s\n' "${OPERATIONS_RESULTS}" | jq -e 'to_entries | any(.value == "timeout")' > /dev/null; then
    echo "::error::One or more operations commands timed out; inspect the retained command logs above."
    FAILED=true
  fi
fi

if [ "${FAILED}" = true ]; then
  exit 1
fi

echo "All Homeboy commands passed"
