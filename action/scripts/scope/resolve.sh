#!/usr/bin/env bash

# Unified scope resolver — replaces determine-pr-base-ref.sh + resolve-test-scope.sh.
# Runs once per action invocation, detects execution context, computes all scope state.
#
# Inputs (env vars):
#   GITHUB_EVENT_NAME  — pull_request, schedule, workflow_dispatch, push
#   BASE_SHA           — PR base commit (from github.event.pull_request.base.sha)
#   SCOPE_INPUT        — unified scope input: "changed" (default) or "full"
#   PR_HEAD_REPO       — full_name of PR head repo (fork detection)
#   GITHUB_REPOSITORY  — full_name of the base repo
#
# Outputs (GITHUB_ENV + GITHUB_OUTPUT):
#   SCOPE_CONTEXT      — pr | push | cron | manual
#   SCOPE_BASE_REF     — merge base SHA (empty for non-PR or full scope)
#   SCOPE_MODE         — changed | full
#   SCOPE_IS_FORK      — true | false
#
# Backward compat (GITHUB_ENV, one release cycle):
#   HOMEBOY_CHANGED_SINCE — same as SCOPE_BASE_REF

set -euo pipefail

SCOPE_INPUT="${SCOPE_INPUT:-changed}"

# ── Step 1: Determine execution context ──

case "${GITHUB_EVENT_NAME:-}" in
  pull_request|pull_request_target)
    SCOPE_CONTEXT="pr"
    ;;
  push)
    SCOPE_CONTEXT="push"
    ;;
  schedule)
    SCOPE_CONTEXT="cron"
    ;;
  workflow_dispatch)
    SCOPE_CONTEXT="manual"
    ;;
  *)
    SCOPE_CONTEXT="manual"
    ;;
esac

# ── Step 2: Fork detection ──

SCOPE_IS_FORK="false"
if [ "${SCOPE_CONTEXT}" = "pr" ]; then
  if [ -n "${PR_HEAD_REPO:-}" ] && [ "${PR_HEAD_REPO}" != "${GITHUB_REPOSITORY}" ]; then
    SCOPE_IS_FORK="true"
    echo "Fork PR detected: ${PR_HEAD_REPO} → ${GITHUB_REPOSITORY}"
  fi
fi

# ── Step 3: Resolve base ref and scope mode ──

SCOPE_BASE_REF=""
SCOPE_MODE="full"

if [ "${SCOPE_CONTEXT}" = "pr" ]; then
  if [ "${SCOPE_INPUT}" = "full" ]; then
    SCOPE_MODE="full"
    echo "Scope forced to full by input"
  elif [ -n "${BASE_SHA:-}" ]; then
    # Fetch enough ancestry for three-dot diff (base...HEAD) to find the merge base.
    # GitHub's default checkout is --depth=1 (shallow), which only has the tip commits.
    echo "Fetching base ancestry for scoped diffs (${BASE_SHA:0:8})..."

    git fetch origin "${BASE_SHA}" --depth=50 2>/dev/null || true
    git fetch --deepen=50 2>/dev/null || true

    if ! git merge-base "${BASE_SHA}" HEAD >/dev/null 2>&1; then
      echo "Merge base not found with depth=50, deepening further..."
      git fetch --deepen=200 2>/dev/null || true

      if ! git merge-base "${BASE_SHA}" HEAD >/dev/null 2>&1; then
        echo "Merge base still not found, unshallowing..."
        git fetch --unshallow 2>/dev/null || true
      fi
    fi

    if git merge-base "${BASE_SHA}" HEAD >/dev/null 2>&1; then
      MERGE_BASE=$(git merge-base "${BASE_SHA}" HEAD)
      echo "Merge base found: ${MERGE_BASE:0:8}"
      SCOPE_BASE_REF="${BASE_SHA}"
      SCOPE_MODE="changed"
    else
      echo "::warning::Could not find merge base — falling back to full scope"
      SCOPE_MODE="full"
    fi
  else
    echo "::warning::PR event but no BASE_SHA provided — falling back to full scope"
    SCOPE_MODE="full"
  fi
else
  # Non-PR events always use full scope
  SCOPE_MODE="full"
fi

# ── Step 4: Write outputs ──

echo "SCOPE_CONTEXT=${SCOPE_CONTEXT}" >> "${GITHUB_ENV}"
echo "SCOPE_BASE_REF=${SCOPE_BASE_REF}" >> "${GITHUB_ENV}"
echo "SCOPE_MODE=${SCOPE_MODE}" >> "${GITHUB_ENV}"
echo "SCOPE_IS_FORK=${SCOPE_IS_FORK}" >> "${GITHUB_ENV}"

echo "scope-context=${SCOPE_CONTEXT}" >> "${GITHUB_OUTPUT}"
echo "scope-base-ref=${SCOPE_BASE_REF}" >> "${GITHUB_OUTPUT}"
echo "scope-mode=${SCOPE_MODE}" >> "${GITHUB_OUTPUT}"
echo "scope-is-fork=${SCOPE_IS_FORK}" >> "${GITHUB_OUTPUT}"

# Backward compat: HOMEBOY_CHANGED_SINCE (consumed by older workflow files)
echo "HOMEBOY_CHANGED_SINCE=${SCOPE_BASE_REF}" >> "${GITHUB_ENV}"

echo "Scope resolved: context=${SCOPE_CONTEXT} mode=${SCOPE_MODE} fork=${SCOPE_IS_FORK} base_ref=${SCOPE_BASE_REF:-(none)}"
