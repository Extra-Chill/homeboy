#!/usr/bin/env bash
set -euo pipefail

# Upload a WordPress plugin/theme release ZIP to the GitHub Release for the
# current tag. Idempotent: re-running for an existing asset uses --clobber so
# partial-success release runs can be safely retried.
#
# This action runs as part of the homeboy release pipeline after the package
# step has produced build/<slug>.zip and the github.release step has created
# the Release entry on GitHub.
#
# Reads the release payload from HOMEBOY_SETTINGS_JSON which homeboy passes
# when invoking extension actions. The payload shape is:
#   { "release": { "tag": "vX.Y.Z", "component_id": "data-machine", ... },
#     "config":  { ... } }
#
# Requires:
#   - gh CLI on PATH (used for both auth check and upload)
#   - GH_TOKEN or gh auth login session
#   - GITHUB_REPOSITORY env (set by GitHub Actions) or git remote origin

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"

if ! command -v gh >/dev/null 2>&1; then
  echo "Error: gh CLI is required to upload release assets" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "Error: jq is required to parse the release payload" >&2
  exit 1
fi

PAYLOAD="${HOMEBOY_SETTINGS_JSON:-}"
if [[ -z "${PAYLOAD}" ]]; then
  echo "Error: HOMEBOY_SETTINGS_JSON is empty (homeboy release pipeline must invoke this action)" >&2
  exit 1
fi

TAG="$(echo "${PAYLOAD}" | jq -r '.release.tag // empty')"
COMPONENT_SLUG="$(echo "${PAYLOAD}" | jq -r '.release.component_id // empty')"

if [[ -z "${TAG}" ]]; then
  echo "Error: release payload did not contain release.tag" >&2
  exit 1
fi

if [[ -z "${COMPONENT_SLUG}" ]]; then
  COMPONENT_SLUG="${HOMEBOY_COMPONENT_ID:-}"
fi

if [[ -z "${COMPONENT_SLUG}" ]]; then
  echo "Error: could not determine component slug for ZIP lookup" >&2
  exit 1
fi

ARTIFACT_PATH="build/${COMPONENT_SLUG}.zip"

if [[ ! -f "${ARTIFACT_PATH}" ]]; then
  echo "Error: expected release artifact at ${ARTIFACT_PATH}, run release.package first" >&2
  exit 1
fi

# Resolve the GitHub repository slug. Prefer the env var set by GitHub Actions;
# fall back to parsing the origin remote so local dry-runs also work.
REPO_SLUG="${GITHUB_REPOSITORY:-}"
if [[ -z "${REPO_SLUG}" ]]; then
  REMOTE_URL="$(git config --get remote.origin.url 2>/dev/null || true)"
  if [[ -n "${REMOTE_URL}" ]]; then
    # Normalize both git@github.com:owner/repo.git and https://github.com/owner/repo(.git)
    REPO_SLUG="$(echo "${REMOTE_URL}" \
      | sed -E 's#^git@github\.com:#https://github.com/#' \
      | sed -E 's#\.git$##' \
      | sed -E 's#^https://github\.com/##')"
  fi
fi

if [[ -z "${REPO_SLUG}" ]]; then
  echo "Error: could not determine GitHub repository (set GITHUB_REPOSITORY or configure git remote origin)" >&2
  exit 1
fi

echo "Uploading ${ARTIFACT_PATH} to ${REPO_SLUG} release ${TAG}..." >&2

gh release upload "${TAG}" "${ARTIFACT_PATH}" \
  --clobber \
  --repo "${REPO_SLUG}" >&2

echo "Uploaded ${ARTIFACT_PATH} to ${REPO_SLUG} release ${TAG}" >&2

# Emit a small JSON receipt so the homeboy pipeline can record this as a
# successful publish step. Match the shape other publish targets use.
jq -cn \
  --arg tag "${TAG}" \
  --arg repo "${REPO_SLUG}" \
  --arg path "${ARTIFACT_PATH}" \
  '{
    target: "github-release-asset",
    tag: $tag,
    repository: $repo,
    artifact_path: $path,
    success: true
  }'
