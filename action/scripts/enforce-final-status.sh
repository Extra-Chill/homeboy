#!/usr/bin/env bash

set -euo pipefail

if [ -z "${RESULTS:-}" ] || [ "${RESULTS}" = "{}" ]; then
  echo "::error::No command results were produced"
  exit 1
fi

if echo "${RESULTS}" | jq -e 'to_entries | any(.value == "fail")' > /dev/null; then
  echo "::error::One or more Homeboy commands failed"
  exit 1
fi

echo "All Homeboy commands passed"
