#!/usr/bin/env bash

set -euo pipefail

if [ -n "${RERUN_RESULTS:-}" ] && [ "${RERUN_RESULTS}" != "{}" ]; then
  echo "results=${RERUN_RESULTS}" >> "${GITHUB_OUTPUT}"
else
  echo "results=${FIRST_RESULTS}" >> "${GITHUB_OUTPUT}"
fi
