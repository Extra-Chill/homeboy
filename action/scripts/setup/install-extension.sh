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
  # The reuse marker goes with it: it only ever describes a tree this script
  # installed and has not since removed.
  rm -f "$(install_marker_path "${extension_id}")" 2>/dev/null || true
  homeboy extension uninstall "${extension_id}" >/dev/null 2>&1 || true
}

# ── Cached install reuse (Extra-Chill/homeboy#18462) ──
#
# `Cache Homeboy` restores the extension store, and refreshing it costs a
# monorepo clone plus the extension's setup (composer/npm for WordPress) on
# every job. When the caller pins an immutable `extension-ref`, a restored tree
# installed from the same source at the same ref is already the requested
# release, so it is reused instead.
#
# Homeboy records the source URL, requested ref and resolved revision inside
# the extension dir (`.source-url`, `.source-requested-ref`,
# `.source-revision`), but writes them before running the extension's setup, so
# they alone do not prove setup finished. After `homeboy extension install`
# succeeds this script writes a marker next to the extension dir
# (`extensions/.<id>.homeboy-action-install`) recording the normalized source,
# the requested ref and the resolved commit (read from Homeboy's
# `.source-revision`). Homeboy deletes the tree when setup fails, so the marker
# exists only for a fully set-up install.
#
# A cached install is reused only when all of these hold:
#   - the requested ref is immutable:
#       * a 7-40 character hex string that the recorded commit starts with
#         (full or abbreviated commit SHA; no network), or
#       * a tag: `git ls-remote <source>` lists refs/tags/<ref> (peeled when
#         annotated), resolves it to the recorded commit, and lists no branch
#         of the same name. A moved tag therefore reinstalls.
#   - the marker's source and ref equal this run's source and ref,
#   - the extension manifest exists and Homeboy's recorded revision, when
#     present, still equals the marker's commit.
# Anything else, including any error while checking, falls back to the fresh
# uninstall + install. Branch refs, an empty ref, install-for-component and the
# legacy-source fallback never reuse.
EXTENSION_REUSE_ALLOWED="${EXTENSION_REUSE_ALLOWED:-true}"

extensions_root() {
  local config_root="${HOMEBOY_CONFIG_ROOT:-}"
  if [ -z "${config_root//[[:space:]]/}" ]; then
    config_root="${HOME}/.config/homeboy"
  fi
  printf '%s\n' "${config_root%/}/extensions"
}

install_marker_path() {
  printf '%s\n' "$(extensions_root)/.${1}.homeboy-action-install"
}

normalize_source() {
  local source="${1%/}"
  printf '%s\n' "${source%.git}"
}

# Homeboy's resolved revision for an installed extension: the embedded
# `.source-revision` (extracted monorepo installs), the sidecar Homeboy writes
# next to a linked install, or the git HEAD of a cloned single-extension repo.
installed_revision() {
  local extension_id="$1"
  local extension_dir
  local candidate
  extension_dir="$(extensions_root)/${extension_id}"

  for candidate in "${extension_dir}/.source-revision" "$(extensions_root)/.${extension_id}.source-revision"; do
    if [ -f "${candidate}" ]; then
      tr -d '[:space:]' < "${candidate}"
      return 0
    fi
  done

  if [ -e "${extension_dir}/.git" ]; then
    git -C "${extension_dir}" rev-parse HEAD 2>/dev/null
    return $?
  fi

  return 1
}

read_marker_field() {
  local marker="$1"
  local field="$2"
  sed -n "s/^${field}=//p" "${marker}" | head -n 1
}

record_install_marker() {
  local extension_id="$1"
  local extension_dir
  local commit
  local marker
  extension_dir="$(extensions_root)/${extension_id}"
  marker="$(install_marker_path "${extension_id}")"

  [ -f "${extension_dir}/${extension_id}.json" ] || return 0
  commit="$(installed_revision "${extension_id}" 2>/dev/null || true)"

  printf 'source=%s\nref=%s\ncommit=%s\n' \
    "$(normalize_source "${EXTENSION_SOURCE}")" "${EXTENSION_REF}" "${commit}" > "${marker}.tmp"
  mv -f "${marker}.tmp" "${marker}"
}

# Prints the commit an immutable ref names, or fails when the ref is mutable
# or cannot be resolved.
tag_commit() {
  local source="$1"
  local ref="$2"
  local listing
  local timeout_cmd=()

  if command -v timeout >/dev/null 2>&1; then
    timeout_cmd=(timeout 60)
  fi

  listing="$(GIT_TERMINAL_PROMPT=0 ${timeout_cmd[@]+"${timeout_cmd[@]}"} git ls-remote "${source}" \
    "refs/heads/${ref}" "refs/tags/${ref}" "refs/tags/${ref}^{}")" || return 1

  # A branch with the same name makes the ref ambiguous, so treat it as mutable.
  if awk -v want="refs/heads/${ref}" '$2 == want { found = 1 } END { exit !found }' <<< "${listing}"; then
    return 1
  fi

  awk -v plain="refs/tags/${ref}" -v peeled="refs/tags/${ref}^{}" '
    $2 == peeled { peeled_sha = $1 }
    $2 == plain { plain_sha = $1 }
    END {
      sha = (peeled_sha != "") ? peeled_sha : plain_sha
      if (sha == "") exit 1
      print sha
    }' <<< "${listing}"
}

# Succeeds only when the cached install of extension_id can be reused. Run it
# in a subshell: any error inside means "do not reuse".
cached_install_reusable() {
  local extension_id="$1"
  local ref="${EXTENSION_REF}"
  local source
  local marker
  local extension_dir
  local marker_commit
  local current_commit
  local resolved

  [ "${EXTENSION_REUSE_ALLOWED}" = "true" ] || return 1
  [ -n "${ref}" ] || return 1

  source="$(normalize_source "${EXTENSION_SOURCE}")"
  marker="$(install_marker_path "${extension_id}")"
  extension_dir="$(extensions_root)/${extension_id}"

  [ -f "${marker}" ] || return 1
  [ -f "${extension_dir}/${extension_id}.json" ] || return 1
  [ "$(read_marker_field "${marker}" source)" = "${source}" ] || return 1
  [ "$(read_marker_field "${marker}" ref)" = "${ref}" ] || return 1

  marker_commit="$(read_marker_field "${marker}" commit | tr '[:upper:]' '[:lower:]')"
  [[ "${marker_commit}" =~ ^[0-9a-f]{40}([0-9a-f]{24})?$ ]] || return 1

  current_commit="$(installed_revision "${extension_id}" 2>/dev/null || true)"
  current_commit="$(printf '%s' "${current_commit}" | tr '[:upper:]' '[:lower:]')"
  if [ -n "${current_commit}" ] && [ "${current_commit}" != "${marker_commit}" ]; then
    return 1
  fi

  local lower_ref
  lower_ref="$(printf '%s' "${ref}" | tr '[:upper:]' '[:lower:]')"
  if [[ "${lower_ref}" =~ ^[0-9a-f]{7,64}$ ]] && [[ "${marker_commit}" == "${lower_ref}"* ]]; then
    printf '%s\n' "${marker_commit}"
    return 0
  fi

  resolved="$(tag_commit "${EXTENSION_SOURCE}" "${ref}")" || return 1
  resolved="$(printf '%s' "${resolved}" | tr '[:upper:]' '[:lower:]')"
  [ "${resolved}" = "${marker_commit}" ] || return 1
  printf '%s\n' "${marker_commit}"
}

# Install extension_id at EXTENSION_REF, reusing a matching cached install.
install_extension_at_ref() {
  local extension_id="$1"
  local reused_commit

  if [ -z "${extension_id}" ]; then
    return 0
  fi

  if reused_commit="$(cached_install_reusable "${extension_id}" 2>/dev/null)"; then
    echo "Reusing cached extension '${extension_id}' from $(normalize_source "${EXTENSION_SOURCE}") at ${EXTENSION_REF} (commit ${reused_commit}); skipping reinstall."
    # `extension install` also runs the extension's setup, which provisions
    # runtime state outside the cached tree (e.g. WordPress installs WP Codebox
    # under ~/.cache and exports HOMEBOY_WP_CODEBOX_BIN to GITHUB_ENV). Reuse
    # skips the download, never the provisioning: a cache hit must leave the
    # job in the same state as a fresh install (Extra-Chill/homeboy#18488).
    homeboy extension setup "${extension_id}"
    return 0
  fi

  refresh_extension "${extension_id}"
  install_extension "${extension_id}"
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

  # Bookkeeping only: a marker that cannot be written just means the next run
  # installs fresh.
  (record_install_marker "${extension_id}") >/dev/null 2>&1 || true
}

install_configured_extensions_at_ref() {
  local config_file="${COMPONENT_DIR}/homeboy.json"

  if [ ! -f "${config_file}" ]; then
    install_extension_at_ref "${EXTENSION_ID}"
    return 0
  fi

  while IFS= read -r configured_extension_id; do
    install_extension_at_ref "${configured_extension_id}"
  done < <(jq -r '.extensions // {} | keys[]' "${config_file}" 2>/dev/null || true)
}

# The extensions moved into extensions/ of Extra-Chill/homeboy
# (Extra-Chill/homeboy#16783). The old repository is frozen but still serves
# what it always did. Two kinds of caller can only install from it:
#   - callers pinning `extension-ref` to a homeboy-extensions commit SHA, which
#     does not exist in the monorepo's rewritten history, and
#   - callers pinning a Homeboy release that predates nested extension roots.
# For them, an install from the default monorepo source that fails is retried
# from the frozen repository, with a warning to move the pin.
MONOREPO_EXTENSION_SOURCE="https://github.com/Extra-Chill/homeboy"
LEGACY_EXTENSION_SOURCE="https://github.com/Extra-Chill/homeboy-extensions"

install_requested_extensions() {
  if [ -n "${EXTENSION_INPUT}" ]; then
    echo "Installing extension override: ${EXTENSION_INPUT} from ${EXTENSION_SOURCE}..."
    if [ -n "${EXTENSION_REF}" ]; then
      install_extension_at_ref "${EXTENSION_INPUT}"
    else
      refresh_extension "${EXTENSION_INPUT}"
      install_extension "${EXTENSION_INPUT}"
    fi
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
}

is_monorepo_extension_source() {
  local source="${1%/}"
  source="${source%.git}"
  [ "${source}" = "${MONOREPO_EXTENSION_SOURCE}" ]
}

if is_monorepo_extension_source "${EXTENSION_SOURCE}"; then
  # errexit is ignored inside an `if` condition, so run the attempt as a plain
  # subshell with errexit on and read its status afterwards.
  set +e
  (set -e; install_requested_extensions)
  monorepo_install_status=$?
  set -e
  if [ "${monorepo_install_status}" -ne 0 ]; then
    echo "::warning::Installing extensions from ${EXTENSION_SOURCE}${EXTENSION_REF:+ at ${EXTENSION_REF}} failed; retrying from the frozen ${LEGACY_EXTENSION_SOURCE}. Pin extension-ref to a ref in Extra-Chill/homeboy (e.g. wordpress-vX.Y.Z) and a Homeboy release with nested extension roots to drop this fallback (Extra-Chill/homeboy#16783)."
    EXTENSION_SOURCE="${LEGACY_EXTENSION_SOURCE}"
    EXTENSION_REUSE_ALLOWED=false
    install_requested_extensions
  fi
else
  install_requested_extensions
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
    # Honors HOMEBOY_CONFIG_ROOT (config-dir), where Homeboy installs
    # extensions. Kept inline: tests extract this function on its own.
    local config_root="${HOMEBOY_CONFIG_ROOT:-}"
    if [ -z "${config_root//[[:space:]]/}" ]; then
        config_root="${HOME}/.config/homeboy"
    fi
    local extensions_root="${config_root%/}/extensions"
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
