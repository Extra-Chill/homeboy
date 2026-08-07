#!/usr/bin/env bash
#
# Emits whether this CI run may admit candidate work. A pull_request.closed run
# exists solely to cancel the previous run in the same concurrency group.

set -euo pipefail

active=true
if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ] \
  && [ "${GITHUB_EVENT_ACTION:-}" = "closed" ]; then
  active=false
fi

echo "active=${active}" >> "${GITHUB_OUTPUT:?GITHUB_OUTPUT must be set}"
