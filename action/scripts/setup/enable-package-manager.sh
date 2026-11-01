#!/usr/bin/env bash
# Make the component's declared Node package manager available on PATH.
#
# actions/setup-node installs Node and npm only. A component that declares
# `"packageManager": "pnpm@x.y.z"` (or yarn) otherwise has no `pnpm`/`yarn`
# binary, and every step that invokes it directly fails with "not found" — the
# Homeboy dependency preflight among them. Corepack ships with Node and
# installs exactly the version the component pins, so the toolchain matches
# what the component's own CI uses.
#
# Inputs (env):
#   COMPONENT_DIR           — component directory relative to the workspace (default ".")
#   COREPACK_INSTALL_DIR    — where corepack writes the package manager shim
#                             (default: corepack's own, next to node on PATH)
#
# A component with no package.json, or no packageManager field, is left as is:
# npm is already present. An unsupported or malformed declaration fails loudly
# rather than silently falling back to a different package manager.

set -euo pipefail

component_dir="${COMPONENT_DIR:-.}"
manifest="${component_dir%/}/package.json"

if [ ! -f "${manifest}" ]; then
  exit 0
fi

declared="$(jq -r '.packageManager // empty' "${manifest}")"
if [ -z "${declared}" ]; then
  exit 0
fi

name="${declared%%@*}"
case "${name}" in
  npm)
    exit 0
    ;;
  pnpm | yarn) ;;
  *)
    echo "::error::${manifest} declares unsupported packageManager '${declared}'" >&2
    exit 1
    ;;
esac

if [ "${declared}" = "${name}" ]; then
  echo "::error::${manifest} declares packageManager '${declared}' without a version; pin one, e.g. ${name}@1.2.3" >&2
  exit 1
fi

enable_args=()
if [ -n "${COREPACK_INSTALL_DIR:-}" ]; then
  enable_args+=(--install-directory "${COREPACK_INSTALL_DIR}")
fi
corepack enable "${enable_args[@]}" "${name}"
(cd "${component_dir}" && corepack install)

installed="$(cd "${component_dir}" && "${name}" --version)"
echo "Enabled ${name} ${installed} from ${manifest} (${declared})"
