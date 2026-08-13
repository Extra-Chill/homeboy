#!/usr/bin/env bash
# Normalize cargo-dist's per-asset checksum output before it becomes a release asset.
set -euo pipefail

ASSET_DIR="${1:?usage: canonicalize-checksum-sidecars.sh ASSET_DIR}"

shopt -s nullglob
for sidecar in "${ASSET_DIR}"/*.sha256; do
  mapfile -t records < "${sidecar}"
  nonempty=()
  for record in "${records[@]}"; do
    [ -z "${record}" ] || nonempty+=("${record}")
  done
  [ "${#nonempty[@]}" -eq 1 ] || {
    echo "::error::Checksum sidecar ${sidecar} must contain one checksum record" >&2
    exit 1
  }

  read -r digest name extra <<< "${nonempty[0]}"
  name="${name#\*}"
  [[ "${digest}" =~ ^[[:xdigit:]]{64}$ ]] && [ -n "${name}" ] && [ -z "${extra:-}" ] \
    && [ "${name}" = "$(basename "${sidecar%.sha256}")" ] || {
      echo "::error::Checksum sidecar ${sidecar} has an invalid payload record" >&2
      exit 1
    }

  printf '%s *%s\n' "${digest,,}" "${name}" > "${sidecar}"
done
