#!/usr/bin/env bash

set -euo pipefail

EXTENSION_INPUT="${EXTENSION_INPUT:-}"
EXTENSION_REF="${EXTENSION_REF:-}"
COMPONENT_DIR="${COMPONENT_DIR:-.}"

if [[ "${EXTENSION_INPUT}" == *,* ]]; then
  echo "::error::Comma-separated extension input is not supported. Declare multiple extensions in homeboy.json instead."
  exit 1
fi

refresh_extension() {
  local extension_id="$1"

  if [ -z "${extension_id}" ]; then
    return 0
  fi

  # GitHub Actions restores ~/.config/homeboy/extensions from cache. Homeboy's
  # install command preserves an existing extension tree, so remove the cached
  # copy first to guarantee each run consumes the current extension release.
  homeboy extension uninstall "${extension_id}" >/dev/null 2>&1 || true
}

refresh_configured_extensions() {
  local config_file="${COMPONENT_DIR}/homeboy.json"

  if [ ! -f "${config_file}" ]; then
    refresh_extension "${EXTENSION_ID}"
    return 0
  fi

  while IFS= read -r configured_extension_id; do
    refresh_extension "${configured_extension_id}"
  done < <(jq -r '.extensions // {} | keys[]' "${config_file}" 2>/dev/null || true)
}

install_extension() {
  local extension_id="$1"
  local args=(extension install "${EXTENSION_SOURCE}" --id "${extension_id}")

  if [ -n "${EXTENSION_REF}" ]; then
    args+=(--ref "${EXTENSION_REF}")
  fi

  homeboy "${args[@]}"
}

install_configured_extensions_at_ref() {
  local config_file="${COMPONENT_DIR}/homeboy.json"

  if [ ! -f "${config_file}" ]; then
    refresh_extension "${EXTENSION_ID}"
    install_extension "${EXTENSION_ID}"
    return 0
  fi

  while IFS= read -r configured_extension_id; do
    refresh_extension "${configured_extension_id}"
    install_extension "${configured_extension_id}"
  done < <(jq -r '.extensions // {} | keys[]' "${config_file}" 2>/dev/null || true)
}

if [ -n "${EXTENSION_INPUT}" ]; then
  echo "Installing extension override: ${EXTENSION_INPUT} from ${EXTENSION_SOURCE}..."
  refresh_extension "${EXTENSION_INPUT}"
  install_extension "${EXTENSION_INPUT}"
  echo "Extension '${EXTENSION_INPUT}' installed successfully"
elif [ -n "${EXTENSION_REF}" ]; then
  echo "Installing configured extensions from ${EXTENSION_SOURCE} at ${EXTENSION_REF}..."
  install_configured_extensions_at_ref
  echo "Configured extensions installed successfully"
else
  if homeboy extension install-for-component --help >/dev/null 2>&1; then
    echo "Installing extensions configured by ${COMPONENT_DIR}/homeboy.json from ${EXTENSION_SOURCE}..."
    refresh_configured_extensions
    homeboy extension install-for-component --path "${COMPONENT_DIR}" --source "${EXTENSION_SOURCE}"
    echo "Configured extensions installed successfully"
  else
    echo "::warning::Installed Homeboy does not support 'extension install-for-component'; falling back to '${EXTENSION_ID}' only"
    refresh_extension "${EXTENSION_ID}"
    install_extension "${EXTENSION_ID}"
    echo "Extension '${EXTENSION_ID}' installed successfully"
  fi
fi

# Publish where the toolchain landed.
#
# The action installs the CLI and its extensions — phpunit, phpcs, wpcs,
# phpstan and the WordPress stubs all live inside an extension — but until
# now it never told the surrounding workflow where any of it was. A `run:`
# step could not invoke `homeboy` or locate an extension's `vendor/bin`, so
# every consumer needing a custom test or lint step had to keep a private
# copy of the same tools, with pins free to drift from the ones CI actually
# runs.
#
# These are facts this script already knows rather than new configuration.
publish_toolchain_location() {
    local extensions_root="${HOME}/.config/homeboy/extensions"
    local homeboy_bin
    local primary_extension="${EXTENSION_INPUT:-${EXTENSION_ID:-}}"

    if [ -z "${GITHUB_ENV:-}" ]; then
        return 0
    fi

    printf '%s\n' "HOMEBOY_EXTENSIONS_ROOT=${extensions_root}" >> "${GITHUB_ENV}"

    # HOMEBOY_EXTENSION_PATH is singular while homeboy.json may declare several
    # extensions, so it names the primary one this invocation resolved. Consumers
    # needing another append its id to HOMEBOY_EXTENSIONS_ROOT.
    if [ -n "${primary_extension}" ] && [ -d "${extensions_root}/${primary_extension}" ]; then
        printf '%s\n' "HOMEBOY_EXTENSION_PATH=${extensions_root}/${primary_extension}" >> "${GITHUB_ENV}"
    fi

    if [ -n "${GITHUB_PATH:-}" ]; then
        homeboy_bin="$(command -v homeboy 2>/dev/null || true)"
        if [ -n "${homeboy_bin}" ]; then
            printf '%s\n' "$(dirname "${homeboy_bin}")" >> "${GITHUB_PATH}"
        fi
    fi
}

publish_toolchain_location
