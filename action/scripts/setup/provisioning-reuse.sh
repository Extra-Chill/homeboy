#!/usr/bin/env bash

# Reuse provisioning across repeated action invocations in one job.
#
# A job that invokes this action more than once (e.g. gate, deploy, verify)
# used to repeat the whole setup each time: version resolution, binary
# install, extension install, runtime detection, PHP/Node setup and the
# extension's dependency setup. Everything those steps produce outlives the
# step: the binary and extensions stay on disk, PHP/Node stay installed, and
# GITHUB_ENV/GITHUB_PATH entries persist for the rest of the job. Only step
# outputs are scoped to one invocation.
#
# `record` writes a marker under RUNNER_TEMP (job-scoped) keyed by every input
# that changes what gets provisioned, holding the outputs later steps need.
# `check` finds the marker for the current inputs, verifies the recorded state
# is still live, and republishes those outputs so provisioning steps can skip.
#
# Usage: provisioning-reuse.sh check|record
#
# check env:
#   PROVISIONING_INPUTS — newline-separated name=value lines (action inputs)
#   COMPONENT_DIR       — component directory holding homeboy.json
# check outputs: key, reuse, plus the recorded values below on reuse.
#
# record env:
#   PROVISIONING_KEY    — key from check
#   BINARY_SOURCE, HOMEBOY_VERSION_OUTPUT, TOOLING_IDENTITY,
#   PORTABLE_PHP_OUTPUT, PORTABLE_NODE_OUTPUT — outputs to republish
#   INSTALLED_EXTENSION — extension id installed by this invocation, if any

set -euo pipefail

mode="${1:-}"
marker_dir="${RUNNER_TEMP:-}/homeboy-action-provisioning"

output() {
  printf '%s=%s\n' "$1" "$2" >> "${GITHUB_OUTPUT}"
}

provisioning_key() {
  local config_file="${COMPONENT_DIR:-.}/homeboy.json"
  local config_digest="none"

  if [ -f "${config_file}" ]; then
    config_digest="$(sha256sum "${config_file}" | cut -d' ' -f1)"
  fi

  # The action path distinguishes action revisions; homeboy.json decides
  # which extensions install-for-component installs.
  printf '%s\naction=%s\nconfig=%s\nrunner=%s-%s\n' \
    "${PROVISIONING_INPUTS:-}" "${GITHUB_ACTION_PATH:-}" "${config_digest}" \
    "$(uname -s)" "$(uname -m)" | sha256sum | cut -d' ' -f1
}

# Prints the value of one name=value field from the marker.
marker_field() {
  local name="$1" file="$2" line
  while IFS= read -r line; do
    if [ "${line%%=*}" = "${name}" ]; then
      printf '%s' "${line#*=}"
      return 0
    fi
  done < "${file}"
  return 0
}

# Identifies what is installed for one extension, so a later invocation that
# reinstalled it from another source or ref is not mistaken for this one.
extension_identity() {
  local dir="${HOME}/.config/homeboy/extensions/$1" name

  [ -n "$1" ] && [ -d "${dir}" ] || return 0
  for name in .source-url .source-revision .source-requested-ref; do
    if [ -f "${dir}/${name}" ]; then
      printf '%s:%s;' "${name}" "$(tr -d '[:space:]' < "${dir}/${name}")"
    fi
  done
  if [ -d "${dir}/.git" ]; then
    printf 'git:%s;' "$(git -C "${dir}" rev-parse HEAD 2>/dev/null || true)"
  fi
  printf 'present'
}

# Confirms the recorded provisioning still exists. A step between invocations
# may have removed or replaced any of it.
live_state_matches() {
  local marker="$1" field

  field="$(marker_field homeboy-path "${marker}")"
  if [ -z "${field}" ] || [ "$(command -v homeboy 2>/dev/null || true)" != "${field}" ]; then
    echo "homeboy binary moved or missing"
    return 1
  fi

  field="$(marker_field homeboy-cli-version "${marker}")"
  if [ "$(homeboy --version 2>/dev/null || true)" != "${field}" ]; then
    echo "homeboy --version changed"
    return 1
  fi

  field="$(marker_field installed-extension "${marker}")"
  if [ -n "${field}" ] && [ "$(extension_identity "${field}")" != "$(marker_field extension-identity "${marker}")" ]; then
    echo "extension '${field}' was removed or reinstalled"
    return 1
  fi

  if [ -n "$(marker_field portable-php "${marker}")" ] && ! command -v php >/dev/null 2>&1; then
    echo "php is no longer on PATH"
    return 1
  fi

  if [ -n "$(marker_field portable-node "${marker}")" ] && ! command -v node >/dev/null 2>&1; then
    echo "node is no longer on PATH"
    return 1
  fi
}

check() {
  local key marker reason field

  if [ -z "${RUNNER_TEMP:-}" ]; then
    output reuse false
    return 0
  fi

  key="$(provisioning_key)"
  marker="${marker_dir}/${key}"
  output key "${key}"

  if [ ! -f "${marker}" ]; then
    echo "No earlier provisioning for these inputs in this job; provisioning."
    output reuse false
    return 0
  fi

  if ! reason="$(live_state_matches "${marker}")"; then
    echo "Earlier provisioning is stale (${reason}); provisioning again."
    rm -f "${marker}"
    output reuse false
    return 0
  fi

  for field in binary-source homeboy-version tooling-identity portable-php portable-node; do
    output "${field}" "$(marker_field "${field}" "${marker}")"
  done
  output reuse true
  echo "Reusing provisioning from an earlier invocation in this job ($(marker_field homeboy-cli-version "${marker}"))."
}

record() {
  local tmp

  if [ -z "${RUNNER_TEMP:-}" ] || [ -z "${PROVISIONING_KEY:-}" ]; then
    return 0
  fi

  mkdir -p "${marker_dir}"
  tmp="$(mktemp "${marker_dir}/.marker.XXXXXX")"
  {
    printf 'homeboy-path=%s\n' "$(command -v homeboy 2>/dev/null || true)"
    printf 'homeboy-cli-version=%s\n' "$(homeboy --version 2>/dev/null || true)"
    printf 'installed-extension=%s\n' "${INSTALLED_EXTENSION:-}"
    printf 'extension-identity=%s\n' "$(extension_identity "${INSTALLED_EXTENSION:-}")"
    printf 'binary-source=%s\n' "${BINARY_SOURCE:-}"
    printf 'homeboy-version=%s\n' "${HOMEBOY_VERSION_OUTPUT:-}"
    printf 'tooling-identity=%s\n' "${TOOLING_IDENTITY:-}"
    printf 'portable-php=%s\n' "${PORTABLE_PHP_OUTPUT:-}"
    printf 'portable-node=%s\n' "${PORTABLE_NODE_OUTPUT:-}"
  } > "${tmp}"
  mv "${tmp}" "${marker_dir}/${PROVISIONING_KEY}"
  echo "Recorded provisioning for reuse by later invocations in this job."
}

case "${mode}" in
  check) check ;;
  record) record ;;
  *)
    echo "usage: provisioning-reuse.sh check|record" >&2
    exit 2
    ;;
esac
