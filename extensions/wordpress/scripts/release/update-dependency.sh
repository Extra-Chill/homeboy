#!/usr/bin/env bash
set -euo pipefail

# Repin a Composer custom-package dependency to a just-released upstream version.
#
# Invoked by homeboy's dependency-aware release cascade
# (`release.update_dependency` action) against a DEPENDENT component's checkout.
# It rewrites the dependent's Composer custom-package entry to point at the
# released upstream tag/sha and regenerates the lockfile, replacing the manual
# composer-pin spelunking operators do by hand.
#
# Working directory: the dependent component's checkout (HOMEBOY_COMPONENT_PATH).
#
# Input (HOMEBOY_SETTINGS_JSON), generic dependency coordinates from core:
#   .dependency.released_id  - upstream component id (informational)
#   .dependency.package      - composer package name to repin (e.g. chubes/php-transformer)
#   .dependency.version      - released upstream version (e.g. 1.4.0)
#   .dependency.tag          - released upstream tag (e.g. v1.4.0)
#   .dependency.sha          - released upstream commit sha
#
# What it rewrites, deterministically, in composer.json:
#   repositories[] (type=="package", package.name==<package>):
#     .package.version           = <version>
#     .package.dist.url          = <repo>/archive/refs/tags/<tag>.zip
#     .package.dist.reference    = <sha>
#     .package.source.reference  = <sha> (when a source block exists)
#   require[<package]] / require-dev[<package>] constraint = <version>
# The same package coordinates are mirrored into composer.lock when present.
#
# Lockfile content-hash refresh runs `composer update <package>` when composer
# is available (and HOMEBOY_SKIP_COMPOSER_UPDATE is unset). The deterministic
# JSON rewrite above always runs so the pin is correct even offline.
#
# Output (stdout): one JSON object describing what changed.

if ! command -v jq >/dev/null 2>&1; then
  echo "Error: jq is required to repin composer dependencies" >&2
  exit 1
fi

PAYLOAD="${HOMEBOY_SETTINGS_JSON:-}"
read_field() {
  printf '%s' "${PAYLOAD}" | jq -r --arg key "$1" '
    (.dependency[$key] // empty)
  ' 2>/dev/null || printf ''
}

PACKAGE="$(read_field package)"
VERSION="$(read_field version)"
TAG="$(read_field tag)"
SHA="$(read_field sha)"

if [[ -z "${PACKAGE}" || -z "${VERSION}" || -z "${TAG}" ]]; then
  echo "Error: dependency.package, dependency.version and dependency.tag are required" >&2
  echo "Received payload: ${PAYLOAD}" >&2
  exit 1
fi

if [[ ! -f composer.json ]]; then
  echo "Error: no composer.json in $(pwd); cannot repin ${PACKAGE}" >&2
  exit 1
fi

# Rewrite composer.json. Fails (exit nonzero) if the package is not declared as
# a custom-package repository, so a misconfigured cascade surfaces loudly rather
# than silently shipping an unchanged pin.
UPDATED_JSON="$(jq \
  --arg package "${PACKAGE}" \
  --arg version "${VERSION}" \
  --arg tag "${TAG}" \
  --arg sha "${SHA}" \
  '
  def repo_base($p):
    ($p.dist.url // "") as $d
    | if ($d | test("/archive/refs/tags/")) then ($d | sub("/archive/refs/tags/.*$"; ""))
      elif (($p.source.url // "") | length) > 0 then ($p.source.url | sub("\\.git$"; ""))
      else "" end;

  def repin_pkg:
    .package.version = $version
    | .package.dist.type = (.package.dist.type // "zip")
    | .package.dist.url = (repo_base(.package) + "/archive/refs/tags/" + $tag + ".zip")
    | .package.dist.reference = $sha
    | (if .package.source then .package.source.reference = $sha else . end);

  ((.repositories // []) | map(select(.type == "package" and .package.name == $package)) | length) as $matches
  | if $matches == 0 then
      error("composer package \($package) is not declared as a custom-package repository")
    else . end
  | .repositories |= map(if (.type == "package" and .package.name == $package) then repin_pkg else . end)
  | (if (.require // {}) | has($package) then .require[$package] = $version else . end)
  | (if (."require-dev" // {}) | has($package) then ."require-dev"[$package] = $version else . end)
  ' composer.json)"

printf '%s\n' "${UPDATED_JSON}" >composer.json
echo "Repinned ${PACKAGE} -> ${VERSION} (${TAG} @ ${SHA:-no-sha}) in composer.json" >&2

LOCK_UPDATED="false"
if [[ -f composer.lock ]]; then
  UPDATED_LOCK="$(jq \
    --arg package "${PACKAGE}" \
    --arg version "${VERSION}" \
    --arg tag "${TAG}" \
    --arg sha "${SHA}" \
    '
    def repo_base($p):
      ($p.dist.url // "") as $d
      | if ($d | test("/archive/refs/tags/")) then ($d | sub("/archive/refs/tags/.*$"; ""))
        elif (($p.source.url // "") | length) > 0 then ($p.source.url | sub("\\.git$"; ""))
        else "" end;

    def repin:
      .version = $version
      | .dist.type = (.dist.type // "zip")
      | .dist.url = (repo_base(.) + "/archive/refs/tags/" + $tag + ".zip")
      | .dist.reference = $sha
      | (if .source then .source.reference = $sha else . end);

    (.packages // []) |= map(if .name == $package then repin else . end)
    | (."packages-dev" // []) |= map(if .name == $package then repin else . end)
    ' composer.lock)"
  printf '%s\n' "${UPDATED_LOCK}" >composer.lock
  LOCK_UPDATED="true"
  echo "Mirrored ${PACKAGE} coordinates into composer.lock" >&2
fi

COMPOSER_REFRESHED="false"
if [[ -z "${HOMEBOY_SKIP_COMPOSER_UPDATE:-}" ]] && command -v composer >/dev/null 2>&1; then
  echo "Refreshing lockfile via composer update ${PACKAGE}..." >&2
  if composer update "${PACKAGE}" --no-interaction --no-scripts --no-audit >&2; then
    COMPOSER_REFRESHED="true"
  else
    echo "Warning: composer update ${PACKAGE} failed; the dependent's build step will reconcile the lockfile" >&2
  fi
fi

jq -cn \
  --arg package "${PACKAGE}" \
  --arg version "${VERSION}" \
  --arg tag "${TAG}" \
  --arg sha "${SHA}" \
  --argjson lock "${LOCK_UPDATED}" \
  --argjson composer "${COMPOSER_REFRESHED}" \
  '{
    success: true,
    package: $package,
    version: $version,
    tag: $tag,
    sha: $sha,
    composer_json_updated: true,
    composer_lock_updated: $lock,
    composer_refreshed: $composer
  }'
