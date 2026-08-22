#!/usr/bin/env bash

# Report whether the pull request is still eligible for candidate finalization.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "${SCRIPT_DIR}/lib.sh"

if ! pr_is_active; then
  echo "::warning::PR #${PR_NUMBER:-unknown} is no longer open (merged or closed) — skipping final enforcement"
  echo "active=false" >> "${GITHUB_OUTPUT}"
else
  echo "active=true" >> "${GITHUB_OUTPUT}"
fi
