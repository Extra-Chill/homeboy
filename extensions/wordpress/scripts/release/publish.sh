#!/usr/bin/env bash
set -euo pipefail

# Upload a WordPress plugin/theme release ZIP to the GitHub Release for the
# current tag, and optionally mirror the unzipped vendor-bundled tree to a
# CORS-friendly branch (e.g. "release-latest") so WordPress Playground
# blueprints can install it via `git:directory` without hitting CORS errors
# on the GitHub release-asset CDN.
#
# Idempotent: re-running for an existing asset uses --clobber so partial-
# success release runs can be safely retried. The release-latest branch is
# always force-pushed to the new tree because it intentionally has no
# linear history — each release replaces the entire branch.
#
# This action runs as part of the homeboy release pipeline after the package
# step has produced build/<slug>.zip and the github.release step has created
# the Release entry on GitHub.
#
# Reads the release payload from HOMEBOY_SETTINGS_JSON which homeboy passes
# when invoking extension actions. The payload shape is:
#   { "release": { "tag": "vX.Y.Z", "component_id": "sample-plugin", ... },
#     "config":  { ... } }
#
# Reads the release-latest branch name from the component's homeboy.json at
# .extensions.wordpress.release_latest_branch. When empty or unset the mirror
# push is skipped — the GitHub release-asset upload always runs. Components
# that want the branch mirror set it to e.g. "release-latest" in their
# homeboy.json:
#
#   {
#     "extensions": {
#       "wordpress": {
#         "release_latest_branch": "release-latest"
#       }
#     }
#   }
#
# Requires:
#   - gh CLI on PATH (used for both auth check and upload)
#   - GH_TOKEN or gh auth login session
#   - GITHUB_REPOSITORY env (set by GitHub Actions) or git remote origin
#   - git on PATH
#   - unzip on PATH (when release_latest_branch is configured)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"

normalize_github_remote() {
  local remote_url="$1"
  local host=""
  local slug=""

  case "${remote_url}" in
    git@*:*)
      host="${remote_url#git@}"
      host="${host%%:*}"
      slug="${remote_url#*:}"
      ;;
    ssh://git@*/*)
      local rest="${remote_url#ssh://git@}"
      host="${rest%%/*}"
      slug="${rest#*/}"
      ;;
    http://*/*|https://*/*)
      local rest="${remote_url#http://}"
      rest="${rest#https://}"
      host="${rest%%/*}"
      host="${host##*@}"
      slug="${rest#*/}"
      ;;
  esac

  slug="${slug%.git}"
  slug="${slug%/}"
  if [[ -n "${host}" && -n "${slug}" && "${slug}" == */* ]]; then
    printf '%s\t%s\n' "${host}" "${slug}"
  fi
}

apply_github_host_env() {
  local host="$1"
  local proxy=""
  proxy="$(echo "${PAYLOAD}" | jq -r --arg host "${host}" '.config.github.hosts[$host].proxy // empty')"

  if [[ "${host}" != "github.com" ]]; then
    export GH_HOST="${host}"
  fi

  if [[ -n "${proxy}" ]]; then
    export HTTPS_PROXY="${proxy}"
  fi

  while IFS=$'\t' read -r key value; do
    [[ -n "${key}" ]] || continue
    if [[ "${key}" == "GH_HOST" ]]; then
      continue
    fi
    if [[ "${key}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
      export "${key}=${value}"
    fi
  done < <(echo "${PAYLOAD}" | jq -r --arg host "${host}" '.config.github.hosts[$host].env // {} | to_entries[] | [.key, .value] | @tsv')
}

resolve_github_token() {
  if [[ -n "${GH_TOKEN:-}" ]]; then
    return 0
  fi

  local token=""
  token="$(gh auth token 2>/dev/null || true)"
  if [[ -n "${token}" ]]; then
    export GH_TOKEN="${token}"
  fi
}

cleanup_provenance_assets() {
  local zip_name="$1"
  local sidecar_name="$2"
  local cleanup_failed=0

  for asset_name in "${zip_name}" "${sidecar_name}"; do
    if ! gh release delete-asset "${TAG}" "${asset_name}" --yes --repo "${REPO_SLUG}" >&2; then
      echo "Error: failed to remove partial release asset ${asset_name}" >&2
      cleanup_failed=1
    fi
  done
  return "${cleanup_failed}"
}

verify_provenance_assets() {
  local zip_name="$1"
  local sidecar_name="$2"
  local assets=""

  assets="$(gh release view "${TAG}" --repo "${REPO_SLUG}" --json assets)" || return 1
  printf '%s' "${assets}" | jq -e --arg zip "${zip_name}" --arg sidecar "${sidecar_name}" '
    [.assets[]?.name] | index($zip) != null and index($sidecar) != null
  ' >/dev/null
}

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

RECOVERY_ARTIFACTS="$(echo "${PAYLOAD}" | jq -c '
  [
    .release.artifacts // []
    | .[]?
    | select(
        type == "object"
        and (.type == "wordpress-zip" or .artifact_type == "wordpress-zip")
        or (
          type == "object"
          and ((.type // .artifact_type // "") == "")
          and ((.path // "") | type == "string" and endswith(".zip"))
        )
      )
  ]
')"

RECOVERY_ARTIFACT_COUNT="$(echo "${RECOVERY_ARTIFACTS}" | jq 'length')"
if [[ "${RECOVERY_ARTIFACT_COUNT}" -gt 1 ]]; then
  echo "Error: release payload contains multiple WordPress ZIP recovery artifacts" >&2
  exit 1
fi

if [[ "${RECOVERY_ARTIFACT_COUNT}" -eq 1 ]]; then
  if ! echo "${RECOVERY_ARTIFACTS}" | jq -e '.[0].path | type == "string" and length > 0' >/dev/null; then
    echo "Error: WordPress ZIP recovery artifact path must be a non-empty string" >&2
    exit 1
  fi
  ARTIFACT_PATH="$(echo "${RECOVERY_ARTIFACTS}" | jq -r '.[0].path')"
else
  ARTIFACT_PATH="build/${COMPONENT_SLUG}.zip"
fi

if [[ ! -f "${ARTIFACT_PATH}" ]]; then
  echo "Error: expected release artifact at ${ARTIFACT_PATH}" >&2
  exit 1
fi

# A component that opted into package provenance must publish its sealed
# sidecar alongside the ZIP. Resolve an explicitly recovered sidecar first;
# otherwise use the deterministic package output name.
RELEASE_PROVENANCE_COMMAND=""
if [[ -f "homeboy.json" ]]; then
  RELEASE_PROVENANCE_COMMAND="$(jq -r '.extensions.wordpress.settings.release_provenance_command // empty' homeboy.json)"
fi
PROVENANCE_SIDECARS="$(echo "${PAYLOAD}" | jq -c '[.release.artifacts // [] | .[]? | select(type == "object" and (.type == "wordpress-provenance" or .artifact_type == "wordpress-provenance"))]')"
PROVENANCE_SIDECAR_COUNT="$(echo "${PROVENANCE_SIDECARS}" | jq 'length')"
if [[ "${PROVENANCE_SIDECAR_COUNT}" -gt 1 ]]; then
  echo "Error: release payload contains multiple WordPress provenance sidecars" >&2
  exit 1
fi
if [[ "${PROVENANCE_SIDECAR_COUNT}" -eq 1 ]]; then
  if ! echo "${PROVENANCE_SIDECARS}" | jq -e '.[0].path | type == "string" and length > 0' >/dev/null; then
    echo "Error: WordPress provenance sidecar path must be a non-empty string" >&2
    exit 1
  fi
  SIDECAR_PATH="$(echo "${PROVENANCE_SIDECARS}" | jq -r '.[0].path')"
elif [[ -n "${RELEASE_PROVENANCE_COMMAND}" ]]; then
  SIDECAR_PATH="${ARTIFACT_PATH%.zip}.provenance.json"
else
  SIDECAR_PATH=""
fi

ZIP_SHA256=""
SIDECAR_SHA256=""
if [[ -n "${SIDECAR_PATH}" ]]; then
  if [[ ! -f "${SIDECAR_PATH}" ]]; then
    echo "Error: expected release provenance sidecar at ${SIDECAR_PATH}" >&2
    exit 1
  fi
  ZIP_SHA256="$(shasum -a 256 "${ARTIFACT_PATH}")"
  ZIP_SHA256="${ZIP_SHA256%% *}"
  SOURCE_VERSION="$(echo "${PAYLOAD}" | jq -r '.release.version // empty')"
  SOURCE_TAG="$(echo "${PAYLOAD}" | jq -r '.release.tag // empty')"
  SOURCE_COMMIT="$(echo "${PAYLOAD}" | jq -r '.release.commit // .release.source_commit // .release.sha // empty')"
  if [[ -z "${SOURCE_VERSION}" || -z "${SOURCE_TAG}" || -z "${SOURCE_COMMIT}" ]]; then
    echo "Error: provenance publish requires release.version, release.tag, and release.commit" >&2
    exit 1
  fi
  if ! jq -e \
    --arg version "${SOURCE_VERSION}" \
    --arg tag "${SOURCE_TAG}" \
    --arg commit "${SOURCE_COMMIT}" \
    --arg zip_sha256 "${ZIP_SHA256}" \
    '
      .homeboy_wordpress_release_provenance as $provenance
      | ($provenance | type == "object")
      and ($provenance.version == 1)
      and ($provenance.source | (type == "object") and (.version == $version) and (.tag == $tag) and (.commit == $commit))
      and ($provenance.zip | (type == "object") and (.path | type == "string") and (.sha256 == $zip_sha256))
    ' "${SIDECAR_PATH}" >/dev/null; then
    echo "Error: release provenance sidecar schema, source, or ZIP digest does not match release payload" >&2
    exit 1
  fi
  SIDECAR_SHA256="$(shasum -a 256 "${SIDECAR_PATH}")"
  SIDECAR_SHA256="${SIDECAR_SHA256%% *}"
  echo "Verified ${SIDECAR_PATH} binds ${ARTIFACT_PATH}" >&2
fi

# Resolve the GitHub repository slug. Prefer the env var set by GitHub Actions;
# fall back to parsing the origin remote so local dry-runs also work.
GITHUB_HOST="${GH_HOST:-github.com}"
REPO_SLUG="${GITHUB_REPOSITORY:-}"
if [[ -n "${GITHUB_SERVER_URL:-}" ]]; then
  GITHUB_HOST="${GITHUB_SERVER_URL#http://}"
  GITHUB_HOST="${GITHUB_HOST#https://}"
  GITHUB_HOST="${GITHUB_HOST%%/*}"
fi
if [[ -z "${REPO_SLUG}" ]]; then
  REMOTE_URL="$(git config --get remote.origin.url 2>/dev/null || true)"
  if [[ -n "${REMOTE_URL}" ]]; then
    NORMALIZED_REMOTE="$(normalize_github_remote "${REMOTE_URL}")"
    if [[ -n "${NORMALIZED_REMOTE}" ]]; then
      GITHUB_HOST="${NORMALIZED_REMOTE%%$'\t'*}"
      REPO_SLUG="${NORMALIZED_REMOTE#*$'\t'}"
    fi
  fi
fi

if [[ -z "${REPO_SLUG}" ]]; then
  echo "Error: could not determine GitHub repository (set GITHUB_REPOSITORY or configure git remote origin)" >&2
  exit 1
fi

apply_github_host_env "${GITHUB_HOST}"
resolve_github_token

# Assert the artifact's internal version matches the release tag before
# uploading. This is the last chokepoint before a ZIP becomes the GitHub
# Release asset that deploys consume — a stale artifact here means silent
# production rollback (sample-plugin v0.14.0 shipped a v0.8.1 zip
# for 6 days). Strip the leading "v" and any monorepo "<component>-v"
# prefix from the tag to get the bare semver.
EXPECTED_VERSION="${TAG##*v}"
bash "${SCRIPT_DIR}/verify-artifact-version.sh" "${ARTIFACT_PATH}" "${EXPECTED_VERSION}" >/dev/null
echo "Verified ${ARTIFACT_PATH} contains version ${EXPECTED_VERSION} (tag ${TAG})" >&2

echo "Uploading ${ARTIFACT_PATH}${SIDECAR_PATH:+ and ${SIDECAR_PATH}} to ${REPO_SLUG} release ${TAG}..." >&2

if [[ -n "${SIDECAR_PATH}" ]]; then
  ZIP_ASSET_NAME="$(basename "${ARTIFACT_PATH}")"
  SIDECAR_ASSET_NAME="$(basename "${SIDECAR_PATH}")"
  # The ZIP is last so a sidecar upload failure can never create a new
  # unprovenanced ZIP. Any failed or unverifiable transaction removes both.
  if ! gh release upload "${TAG}" "${SIDECAR_PATH}" --clobber --repo "${REPO_SLUG}" >&2; then
    if cleanup_provenance_assets "${ZIP_ASSET_NAME}" "${SIDECAR_ASSET_NAME}"; then
      echo "Error: provenance sidecar upload failed; partial assets were removed" >&2
    else
      echo "Error: provenance sidecar upload failed and rollback was incomplete" >&2
    fi
    exit 1
  fi
  if ! gh release upload "${TAG}" "${ARTIFACT_PATH}" --clobber --repo "${REPO_SLUG}" >&2; then
    if cleanup_provenance_assets "${ZIP_ASSET_NAME}" "${SIDECAR_ASSET_NAME}"; then
      echo "Error: release ZIP upload failed; partial assets were removed" >&2
    else
      echo "Error: release ZIP upload failed and rollback was incomplete" >&2
    fi
    exit 1
  fi
  if ! verify_provenance_assets "${ZIP_ASSET_NAME}" "${SIDECAR_ASSET_NAME}"; then
    if cleanup_provenance_assets "${ZIP_ASSET_NAME}" "${SIDECAR_ASSET_NAME}"; then
      echo "Error: provenance release assets could not be verified; partial assets were removed" >&2
    else
      echo "Error: provenance release assets could not be verified and rollback was incomplete" >&2
    fi
    exit 1
  fi
else
  gh release upload "${TAG}" "${ARTIFACT_PATH}" --clobber --repo "${REPO_SLUG}" >&2
fi

echo "Uploaded ${ARTIFACT_PATH} to ${REPO_SLUG} release ${TAG}" >&2

# Optional: mirror the unzipped vendor-bundled tree to a CORS-friendly branch
# so WordPress Playground blueprints can install via git:directory without
# the release-asset CDN's missing Access-Control-Allow-Origin header.
RELEASE_LATEST_BRANCH=""
if [[ -f "homeboy.json" ]]; then
  RELEASE_LATEST_BRANCH="$(jq -r '.extensions.wordpress.release_latest_branch // ""' homeboy.json)"
fi

if [[ -z "${RELEASE_LATEST_BRANCH}" ]]; then
  echo "release_latest_branch not configured; skipping branch mirror" >&2
  PUSHED_BRANCH=""
else
  if ! command -v unzip >/dev/null 2>&1; then
    echo "Error: unzip is required to mirror the release tree to ${RELEASE_LATEST_BRANCH}" >&2
    exit 1
  fi

  if [[ -z "${GH_TOKEN:-}" ]]; then
    # The release pipeline always exports GH_TOKEN, but make the failure
    # mode obvious for anyone running this script outside the standard CI
    # context.
    echo "Error: GH_TOKEN is required to push to ${RELEASE_LATEST_BRANCH}" >&2
    exit 1
  fi

  echo "Mirroring ${ARTIFACT_PATH} to ${REPO_SLUG} branch ${RELEASE_LATEST_BRANCH}..." >&2

  MIRROR_WORKTREE="$(mktemp -d -t homeboy-wp-release-mirror.XXXXXX)"
  trap 'rm -rf "${MIRROR_WORKTREE}"' EXIT

  unzip -q "${ARTIFACT_PATH}" -d "${MIRROR_WORKTREE}/extracted"

  # The build script wraps the plugin/theme in a directory named after the
  # component slug. The mirror branch should expose the contents of that
  # wrapper at its root so consumers using `git:directory` get a directly
  # installable WordPress plugin/theme tree.
  ZIP_ROOT="${MIRROR_WORKTREE}/extracted/${COMPONENT_SLUG}"
  if [[ ! -d "${ZIP_ROOT}" ]]; then
    # Fall back to the only top-level entry when the ZIP root name does not
    # match the component slug (rare; happens for renames mid-flight).
    candidates=("${MIRROR_WORKTREE}/extracted"/*)
    if [[ ${#candidates[@]} -eq 1 ]] && [[ -d "${candidates[0]}" ]]; then
      ZIP_ROOT="${candidates[0]}"
    else
      echo "Error: could not locate plugin/theme root inside ${ARTIFACT_PATH}" >&2
      exit 1
    fi
  fi

  # Initialise a single-commit orphan branch in a separate working directory
  # so we never touch the source repo's index or HEAD. The branch
  # intentionally has no shared history with main; each release replaces
  # the entire branch contents.
  MIRROR_REPO="${MIRROR_WORKTREE}/repo"
  mkdir -p "${MIRROR_REPO}"
  git -C "${MIRROR_REPO}" init -q -b "${RELEASE_LATEST_BRANCH}"
  git -C "${MIRROR_REPO}" config user.email "homeboy-release@example.invalid"
  git -C "${MIRROR_REPO}" config user.name "Homeboy Release"

  # Copy the extracted tree into the temporary repo. cp -a preserves modes
  # and symlinks; the trailing /. copies contents rather than the directory.
  cp -a "${ZIP_ROOT}/." "${MIRROR_REPO}/"

  git -C "${MIRROR_REPO}" add -A
  git -C "${MIRROR_REPO}" commit -q -m "Release ${TAG}

Mirror of release ZIP ${ARTIFACT_PATH} for ${COMPONENT_SLUG}.
Generated by homeboy-extensions/wordpress release.publish.
Branch is force-pushed on every release; do not commit here directly."

  # Push using a token-authenticated HTTPS URL. We never reuse the source
  # repo's remote so the source checkout's credentials are untouched.
  REMOTE_AUTHENTICATED_URL="https://x-access-token:${GH_TOKEN}@${GITHUB_HOST}/${REPO_SLUG}.git"

  git -C "${MIRROR_REPO}" push --force "${REMOTE_AUTHENTICATED_URL}" \
    "${RELEASE_LATEST_BRANCH}:${RELEASE_LATEST_BRANCH}" >&2

  echo "Mirrored ${ARTIFACT_PATH} to ${REPO_SLUG} branch ${RELEASE_LATEST_BRANCH}" >&2
  PUSHED_BRANCH="${RELEASE_LATEST_BRANCH}"
fi

# Emit a JSON receipt for the homeboy pipeline. Include the mirror branch
# when one was pushed so downstream tooling can verify the dual-publish
# without re-reading homeboy.json.
jq -cn \
  --arg tag "${TAG}" \
  --arg host "${GITHUB_HOST}" \
  --arg repo "${REPO_SLUG}" \
  --arg path "${ARTIFACT_PATH}" \
  --arg sidecar_path "${SIDECAR_PATH}" \
  --arg zip_sha256 "${ZIP_SHA256}" \
  --arg sidecar_sha256 "${SIDECAR_SHA256}" \
  --arg branch "${PUSHED_BRANCH}" \
  '{
    target: "github-release-asset",
    tag: $tag,
    github_host: $host,
    repository: $repo,
    artifact_path: $path,
    provenance: (if $sidecar_path == "" then null else {sidecar_path: $sidecar_path, zip_sha256: $zip_sha256, sidecar_sha256: $sidecar_sha256} end),
    release_latest_branch: (if $branch == "" then null else $branch end),
    success: true
  }'
