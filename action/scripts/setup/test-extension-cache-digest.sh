#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

store="${tmp}/home/.store/_content"
dists="${tmp}/home/.cache/pm"
paths="${store}"$'\n'"${dists}"

value() { sed -n "s/^$1=//p" "$2" | tail -1; }
fail() { echo "FAIL: $*" >&2; exit 1; }

digest() {
  local mode="$1" restored="$2" out="$3"
  CACHE_PATHS="${paths}" RESTORED_DIGEST="${restored}" GITHUB_OUTPUT="${out}" \
    bash "${SCRIPT_DIR}/extension-cache-digest.sh" "${mode}" >/dev/null
}

# A cold runner: nothing restored, nothing to save if commands add nothing.
digest snapshot "" "${tmp}/cold"
[ -z "$(value digest "${tmp}/cold")" ] || fail 'empty stores must have an empty digest'
[ "$(value files "${tmp}/cold")" = "0" ] || fail 'empty stores must count zero files'
digest plan-save "" "${tmp}/cold-plan"
[ "$(value save "${tmp}/cold-plan")" = "false" ] || fail 'empty stores must not be saved'

# Installs filled the stores: save a new entry keyed by the content digest.
mkdir -p "${store}/sha512/aa" "${dists}/files/vendor"
printf 'tarball\n' > "${store}/sha512/aa/one"
printf 'dist\n' > "${dists}/files/vendor/pkg.zip"
digest plan-save "" "${tmp}/filled"
filled="$(value digest "${tmp}/filled")"
[[ "${filled}" =~ ^[0-9a-f]{64}$ ]] || fail "expected a sha256 digest, got '${filled}'"
[ "$(value files "${tmp}/filled")" = "2" ] || fail 'expected two cached files'
[ "$(value save "${tmp}/filled")" = "true" ] || fail 'filled stores must be saved'

# Restored and unchanged: no duplicate save. Rewritten metadata (same paths,
# different bytes) is not new content in a content-addressed store.
digest snapshot "" "${tmp}/restored"
restored="$(value digest "${tmp}/restored")"
[ "${restored}" = "${filled}" ] || fail 'digest must be stable for identical stores'
printf 'tarball, index rewritten\n' > "${store}/sha512/aa/one"
digest plan-save "${restored}" "${tmp}/unchanged"
[ "$(value save "${tmp}/unchanged")" = "false" ] || fail 'unchanged paths must not be saved again'

# A new download adds a path: the cache rolls forward under a new key.
mkdir -p "${store}/sha512/bb"
printf 'another tarball\n' > "${store}/sha512/bb/two"
digest plan-save "${restored}" "${tmp}/grown"
grown="$(value digest "${tmp}/grown")"
[ "$(value save "${tmp}/grown")" = "true" ] || fail 'new downloads must be saved'
[ "${grown}" != "${restored}" ] || fail 'new downloads must change the cache key'

# The same file name in a different store is different content.
mv "${dists}/files/vendor/pkg.zip" "${store}/pkg.zip"
mkdir -p "${dists}/files/vendor" && printf 'x\n' > "${dists}/files/vendor/other.zip"
digest snapshot "" "${tmp}/moved"
[ "$(value digest "${tmp}/moved")" != "${grown}" ] || fail 'store membership must be part of the digest'

if CACHE_PATHS="${paths}" GITHUB_OUTPUT="${tmp}/bad" bash "${SCRIPT_DIR}/extension-cache-digest.sh" bogus 2>/dev/null; then
  fail 'unknown mode must be rejected'
fi

echo 'Extension cache digest tests passed'
