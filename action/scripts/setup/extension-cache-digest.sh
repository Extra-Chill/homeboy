#!/usr/bin/env bash
# Content digest for a rolling extension CI cache.
#
# Rolling caches hold content-addressed download stores (package-manager
# caches). GitHub Actions never overwrites an existing cache key, so a rolling
# cache is saved under a key derived from what it contains: a new entry only
# when the installs added files, and no duplicate when nothing changed.
#
# The digest covers relative file paths only. Stores like these name files by
# content hash, so new downloads always add paths, while index rewrites, mtimes
# and appended metadata do not trigger a new save.
#
# Usage:
#   CACHE_PATHS=<newline list> extension-cache-digest.sh snapshot
#     -> digest=<sha256 or empty>, files=<count>
#   CACHE_PATHS=<newline list> RESTORED_DIGEST=<digest> extension-cache-digest.sh plan-save
#     -> digest=<sha256 or empty>, files=<count>, save=true|false
set -euo pipefail

mode="${1:-snapshot}"
paths="${CACHE_PATHS:-}"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | cut -d' ' -f1
  else
    shasum -a 256 | cut -d' ' -f1
  fi
}

listing=""
files=0
while IFS= read -r cache_path; do
  [ -n "${cache_path}" ] || continue
  [ -d "${cache_path}" ] || continue
  entries="$(cd "${cache_path}" && find . -type f | LC_ALL=C sort)"
  [ -n "${entries}" ] || continue
  count="$(printf '%s\n' "${entries}" | wc -l | tr -d ' ')"
  files=$((files + count))
  listing="${listing}== ${cache_path}"$'\n'"${entries}"$'\n'
done <<< "${paths}"

digest=""
if [ "${files}" -gt 0 ]; then
  digest="$(printf '%s' "${listing}" | sha256)"
fi

{
  echo "digest=${digest}"
  echo "files=${files}"
} >> "${GITHUB_OUTPUT}"

case "${mode}" in
  snapshot)
    echo "Rolling cache holds ${files} files."
    ;;
  plan-save)
    save=false
    if [ -n "${digest}" ] && [ "${digest}" != "${RESTORED_DIGEST:-}" ]; then
      save=true
    fi
    echo "save=${save}" >> "${GITHUB_OUTPUT}"
    if [ "${save}" = "true" ]; then
      echo "Rolling cache changed (${files} files); saving a new entry."
    else
      echo "Rolling cache unchanged (${files} files); nothing to save."
    fi
    ;;
  *)
    echo "::error::Unknown extension cache digest mode '${mode}'." >&2
    exit 1
    ;;
esac
