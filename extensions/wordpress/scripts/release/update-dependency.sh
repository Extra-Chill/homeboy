#!/usr/bin/env bash
set -euo pipefail

# Update one Composer dependency transactionally. Composer is the authority for
# composer.lock; this script only adapts the existing inline package metadata.

if ! command -v jq >/dev/null 2>&1; then
  echo "Error: jq is required to update Composer dependencies" >&2
  exit 1
fi
if ! command -v composer >/dev/null 2>&1; then
  echo "Error: composer is required to update Composer dependencies" >&2
  exit 1
fi
if [[ ! -f composer.json ]]; then
  echo "Error: no composer.json in $(pwd)" >&2
  exit 1
fi

PAYLOAD="${HOMEBOY_SETTINGS_JSON:-}"
read_field() {
  printf '%s' "${PAYLOAD}" | jq -er --arg key "$1" '.dependency[$key] // empty' 2>/dev/null || true
}

PACKAGE="$(read_field package)"
REQUESTED_VERSION="$(read_field version)"
TAG="$(read_field tag)"
SHA="$(read_field sha)"
EXPECTED_SOURCE="$(read_field expected_source)"
EXPECTED_SOURCE_SHA="$(read_field expected_source_sha)"
LATEST_STABLE="$(read_field latest_stable)"
DISCOVERY_CONSTRAINT="$(read_field discovery_constraint)"
ALLOW_CONSTRAINT_REPLACEMENT="$(read_field allow_constraint_replacement)"
MODE="$(read_field mode)"

if [[ -z "${PACKAGE}" ]]; then
  echo "Error: dependency.package is required" >&2
  exit 1
fi
if [[ -z "${REQUESTED_VERSION}" && "${LATEST_STABLE}" != "true" ]]; then
  echo "Error: dependency.version is required unless dependency.latest_stable is true" >&2
  exit 1
fi
if [[ "${REQUESTED_VERSION}" == "latest" ]]; then
  REQUESTED_VERSION=""
  LATEST_STABLE="true"
fi

if ! jq -e --arg package "${PACKAGE}" '
  ((.require // {}) | has($package)) or ((."require-dev" // {}) | has($package))
' composer.json >/dev/null; then
  echo "Error: ${PACKAGE} is not declared in require or require-dev" >&2
  exit 1
fi

CURRENT_VERSION="$(jq -r --arg package "${PACKAGE}" '(.require[$package] // ."require-dev"[$package] // "")' composer.json)"
INLINE_COUNT="$(jq -r --arg package "${PACKAGE}" '[.repositories[]? | select(.type == "package" and .package.name == $package)] | length' composer.json)"
TARGET_REQUIREMENT="${REQUESTED_VERSION}"
FINALIZE_DISCOVERY="false"
if [[ "${LATEST_STABLE}" == "true" ]]; then
  if [[ "${INLINE_COUNT}" != "0" ]]; then
    echo "Error: latest stable discovery is unavailable for one-item inline package metadata; provide exact published tag/version coordinates" >&2
    exit 1
  fi
  if [[ "${CURRENT_VERSION}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    if [[ -z "${DISCOVERY_CONSTRAINT}" || "${ALLOW_CONSTRAINT_REPLACEMENT}" != "true" ]]; then
      echo "Error: exact Composer pins require dependency.discovery_constraint and dependency.allow_constraint_replacement=true for latest stable discovery" >&2
      exit 1
    fi
    TARGET_REQUIREMENT="${DISCOVERY_CONSTRAINT}"
    FINALIZE_DISCOVERY="true"
  elif [[ -n "${DISCOVERY_CONSTRAINT}" ]]; then
    TARGET_REQUIREMENT="${DISCOVERY_CONSTRAINT}"
  else
    TARGET_REQUIREMENT=""
  fi
fi

TMP_DIR="$(mktemp -d "${PWD}/.homeboy-composer-update.XXXXXX")"
cleanup() {
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

cp composer.json "${TMP_DIR}/composer.json"
if [[ -f composer.lock ]]; then
  cp composer.lock "${TMP_DIR}/composer.lock"
fi

# Update only the requirement and the existing inline package coordinates. The
# source URL is retained; package-specific tags therefore remain intact.
jq \
  --arg package "${PACKAGE}" \
  --arg version "${TARGET_REQUIREMENT}" \
  --arg tag "${TAG}" \
  --arg sha "${SHA}" \
  --argjson update_requirement "$( [[ -n "${TARGET_REQUIREMENT}" ]] && printf true || printf false )" \
  '
  def archive_url($p):
    ($p.dist.url // "") as $url
    | if ($url | test("/archive/refs/tags/")) then
        $url | sub("/archive/refs/tags/.*$"; "/archive/refs/tags/" + $tag + ".zip")
      else $url end;
  def update_inline:
    .package.version = $version
    | (if ($tag != "") then .package.dist.url = archive_url(.package) else . end)
    | (if ($sha != "") then .package.dist.reference = $sha else . end)
    | (if ($sha != "" and .package.source) then .package.source.reference = $sha else . end);
  (if ($update_requirement and ((.require // {}) | has($package))) then .require[$package] = $version else . end)
  | (if ($update_requirement and ((."require-dev" // {}) | has($package))) then ."require-dev"[$package] = $version else . end)
  | .repositories = ((.repositories // []) | map(
      if (.type == "package" and .package.name == $package and $update_requirement) then update_inline else . end
    ))
  ' composer.json >"${TMP_DIR}/composer.json"

LOCK_VERSION="$(jq -r --arg package "${PACKAGE}" '((.packages // []) + (."packages-dev" // []))[]? | select(.name == $package) | .version' composer.lock 2>/dev/null | head -n 1 || true)"

if [[ -n "${REQUESTED_VERSION}" && -n "${LOCK_VERSION}" ]] && php -r \
  'exit(version_compare($argv[1], $argv[2], ">") ? 0 : 1);' \
  "${LOCK_VERSION}" "${REQUESTED_VERSION}"; then
  echo "Error: refusing to downgrade ${PACKAGE} from ${LOCK_VERSION} to ${REQUESTED_VERSION}" >&2
  exit 1
fi

if [[ "${MODE}" != "verify" && "${MODE}" != "dry-run" && -n "${REQUESTED_VERSION}" && "${CURRENT_VERSION}" == "${REQUESTED_VERSION}" && "${LOCK_VERSION}" == "${REQUESTED_VERSION}" ]]; then
  SOURCE_OK="true"
  SHA_OK="true"
  INLINE_METADATA_OK="true"
  if [[ -n "${EXPECTED_SOURCE}" ]] && [[ "$(jq -r --arg package "${PACKAGE}" '((.packages // []) + (."packages-dev" // []))[]? | select(.name == $package) | (.source.url // .dist.url // "")' composer.lock 2>/dev/null | head -n 1)" != "${EXPECTED_SOURCE}" ]]; then
    SOURCE_OK="false"
  fi
  if [[ -n "${EXPECTED_SOURCE_SHA}" ]] && ! jq -e --arg package "${PACKAGE}" --arg expected "${EXPECTED_SOURCE_SHA}" '(((.packages // []) + (."packages-dev" // []))[] | select(.name == $package) | ((.source.reference // "") == $expected or (.dist.reference // "") == $expected))' composer.lock >/dev/null; then
    SHA_OK="false"
  fi
  if [[ "${INLINE_COUNT}" != "0" ]] && ! jq -e --arg package "${PACKAGE}" --arg tag "${TAG}" --arg sha "${SHA}" '
    [ .repositories[]? | select(.type == "package" and .package.name == $package) | .package ]
    | length == 1
    and (.[0].version == $version)
    and ($tag == "" or ((.[0].dist.url // "") | contains($tag)))
    and ($sha == "" or ((.[0].dist.reference // "") == $sha and (.[0].source.reference // "") == $sha))
  ' --arg version "${REQUESTED_VERSION}" composer.json >/dev/null; then
    INLINE_METADATA_OK="false"
  fi
  if [[ "${SOURCE_OK}" == "true" && "${SHA_OK}" == "true" && "${INLINE_METADATA_OK}" == "true" ]]; then
    jq -cn --arg package "${PACKAGE}" --arg version "${REQUESTED_VERSION}" \
      '{success:true, package:$package, version:$version, changed:false, composer_refreshed:false}'
    exit 0
  fi
fi

COMPOSER_ARGS=(update "${PACKAGE}" --no-interaction --no-scripts --no-progress --prefer-stable)
if [[ "${FINALIZE_DISCOVERY}" == "true" ]]; then
  COMPOSER_ARGS+=(--with-all-dependencies)
fi
if ! (cd "${TMP_DIR}" && composer "${COMPOSER_ARGS[@]}" >&2); then
  echo "Error: composer update ${PACKAGE} failed; composer.json and composer.lock were left unchanged" >&2
  exit 1
fi

RESOLVED_VERSION="$(jq -r --arg package "${PACKAGE}" '((.packages // []) + (."packages-dev" // []))[]? | select(.name == $package) | .version' "${TMP_DIR}/composer.lock" | head -n 1 || true)"
if [[ -z "${RESOLVED_VERSION}" || "${RESOLVED_VERSION}" == "null" ]]; then
  echo "Error: Composer did not resolve ${PACKAGE}" >&2
  exit 1
fi
if [[ -n "${REQUESTED_VERSION}" && "${RESOLVED_VERSION}" != "${REQUESTED_VERSION}" ]]; then
  echo "Error: Composer resolved ${PACKAGE} to ${RESOLVED_VERSION}, expected ${REQUESTED_VERSION}" >&2
  exit 1
fi
if [[ "${LATEST_STABLE}" == "true" ]]; then
  if ! php -r 'exit(preg_match("/^[vV]?\\d+\\.\\d+\\.\\d+(?:\\.\\d+)?$/", $argv[1]) === 1 ? 0 : 1);' "${RESOLVED_VERSION}"; then
    echo "Error: latest stable discovery resolved prerelease ${PACKAGE} ${RESOLVED_VERSION}" >&2
    exit 1
  fi
  if [[ -n "${LOCK_VERSION}" ]] && php -r \
    'exit(version_compare($argv[1], $argv[2], ">") ? 0 : 1);' \
    "${LOCK_VERSION}" "${RESOLVED_VERSION}"; then
    echo "Error: latest stable discovery would downgrade ${PACKAGE} from ${LOCK_VERSION} to ${RESOLVED_VERSION}" >&2
    exit 1
  fi
fi
if [[ -n "${EXPECTED_SOURCE_SHA}" ]] && ! jq -e --arg package "${PACKAGE}" --arg expected "${EXPECTED_SOURCE_SHA}" '
  (((.packages // []) + (."packages-dev" // []))[] | select(.name == $package) | ((.source.reference // "") == $expected or (.dist.reference // "") == $expected))
' "${TMP_DIR}/composer.lock" >/dev/null; then
  echo "Error: resolved ${PACKAGE} mirror source reference does not match dependency.expected_source_sha" >&2
  exit 1
fi
if [[ -n "${EXPECTED_SOURCE}" ]] && ! jq -e --arg package "${PACKAGE}" --arg expected "${EXPECTED_SOURCE}" '
  (((.packages // []) + (."packages-dev" // []))[] | select(.name == $package) | (.source.url // .dist.url // "")) == $expected
' "${TMP_DIR}/composer.lock" >/dev/null; then
  echo "Error: resolved ${PACKAGE} source does not match dependency.expected_source" >&2
  exit 1
fi

if [[ "${FINALIZE_DISCOVERY}" == "true" ]]; then
  jq --arg package "${PACKAGE}" --arg version "${RESOLVED_VERSION}" '
    if ((.require // {}) | has($package)) then .require[$package] = $version
    elif ((."require-dev" // {}) | has($package)) then ."require-dev"[$package] = $version
    else . end
  ' "${TMP_DIR}/composer.json" >"${TMP_DIR}/composer.json.final"
  mv "${TMP_DIR}/composer.json.final" "${TMP_DIR}/composer.json"
  if ! (cd "${TMP_DIR}" && composer "${COMPOSER_ARGS[@]}" >&2); then
    echo "Error: Composer could not finalize ${PACKAGE} at ${RESOLVED_VERSION}; composer.json and composer.lock were left unchanged" >&2
    exit 1
  fi
fi

if ! (cd "${TMP_DIR}" && composer validate --no-check-publish --check-lock --no-interaction >&2); then
  echo "Error: Composer lock validation failed; composer.json and composer.lock were left unchanged" >&2
  exit 1
fi

if [[ "${MODE}" == "verify" || "${MODE}" == "dry-run" ]]; then
  jq -cn --arg package "${PACKAGE}" --arg version "${RESOLVED_VERSION}" \
    '{success:true, package:$package, version:$version, changed:false, verification_only:true, composer_refreshed:true}'
  exit 0
fi

mv "${TMP_DIR}/composer.json" composer.json
mv "${TMP_DIR}/composer.lock" composer.lock

jq -cn \
  --arg package "${PACKAGE}" \
  --arg version "${RESOLVED_VERSION}" \
  '{success:true, package:$package, version:$version, changed:true, composer_refreshed:true}'
