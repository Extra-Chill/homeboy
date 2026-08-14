#!/usr/bin/env bash
# Normalize cargo-dist's per-asset checksum output before it becomes a release asset.
#
# This runs on every release target, including macOS. macOS ships bash 3.2, so
# bash 4+ builtins are unavailable here no matter what the Linux runners have:
# `mapfile` aborts the job with `command not found` (exit 127), and `${var,,}`
# is a bad substitution. Both were used below, which failed both Darwin builds,
# skipped `Create GitHub Release`, and left the already-pushed tag orphaned --
# homeboy published no release for v0.345.1 through v0.345.5 as a result.
#
# Keep this script POSIX-ish and bash-3.2-clean.
set -euo pipefail

ASSET_DIR="${1:?usage: canonicalize-checksum-sidecars.sh ASSET_DIR}"

shopt -s nullglob
for sidecar in "${ASSET_DIR}"/*.sha256; do
  # bash 3.2 has no `mapfile`. The `|| [ -n "${line}" ]` keeps a final line that
  # is not newline-terminated, which `mapfile` would also have retained.
  nonempty=()
  while IFS= read -r line || [ -n "${line}" ]; do
    [ -z "${line}" ] || nonempty+=("${line}")
  done < "${sidecar}"

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

  # bash 3.2 has no `${var,,}` case expansion.
  digest_lower="$(printf '%s' "${digest}" | tr '[:upper:]' '[:lower:]')"
  printf '%s *%s\n' "${digest_lower}" "${name}" > "${sidecar}"
done
