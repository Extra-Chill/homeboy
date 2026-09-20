#!/usr/bin/env bash

# Read homeboy.json for component identity (ID, extension, directory).
#
# This runs early (Phase 1) before homeboy is installed, so it only reads
# the minimal fields needed for cache keys and command routing. Runtime
# version detection (PHP, Node) is handled later by detect-runtime-env.sh
# after the homeboy binary is available.
#
# Outputs:
#   PORTABLE_ID         — component id
#   PORTABLE_EXTENSION  — explicit extension input, or first key from extensions object
#   COMPONENT_DIR       — directory containing homeboy.json
#
# All values are written to GITHUB_ENV and GITHUB_OUTPUT for use by subsequent steps.

set -euo pipefail

# ── Resolve config path ──
# When COMPONENT_NAME is set and a homeboy.json exists in that subdirectory,
# read config from there instead of the repo root. This supports multi-component
# repos where each component has its own homeboy.json.

COMPONENT_NAME="${COMPONENT_NAME:-}"
CONFIG_DIR="."

if [ -n "${COMPONENT_NAME}" ] && [ -f "${COMPONENT_NAME}/homeboy.json" ]; then
  CONFIG_DIR="${COMPONENT_NAME}"
  echo "Reading config from component subdirectory: ${CONFIG_DIR}/"
elif [ ! -f "homeboy.json" ]; then
  echo "::error::homeboy.json is required at repository root (or in component subdirectory when component input is set)"
  echo "::error::Create homeboy.json with at least an \"id\" field and re-run CI"
  exit 1
fi

CONFIG_FILE="${CONFIG_DIR}/homeboy.json"

# ── Component ID ──

PORTABLE_ID="$(jq -r '.id // empty' "${CONFIG_FILE}" 2>/dev/null || true)"
if [ -z "${PORTABLE_ID}" ]; then
  echo "::error::${CONFIG_FILE} must include a top-level \"id\" field"
  exit 1
fi

# ── Extension ──
# If the action input specifies an extension, use that single override. Otherwise
# infer the first key from the extensions object for downstream runtime detection.
# Installation itself uses Homeboy core to install every configured extension.

EXTENSION_INPUT="${EXTENSION_INPUT:-}"
if [ -n "${EXTENSION_INPUT}" ]; then
  PORTABLE_EXTENSION="${EXTENSION_INPUT}"
else
  PORTABLE_EXTENSION="$(jq -r '.extensions // {} | keys | first // empty' "${CONFIG_FILE}" 2>/dev/null || true)"
fi

# ── Write outputs ──

echo "PORTABLE_ID=${PORTABLE_ID}" >> "${GITHUB_ENV}"
echo "PORTABLE_EXTENSION=${PORTABLE_EXTENSION}" >> "${GITHUB_ENV}"
echo "COMPONENT_DIR=${CONFIG_DIR}" >> "${GITHUB_ENV}"

echo "portable-id=${PORTABLE_ID}" >> "${GITHUB_OUTPUT}"
echo "portable-extension=${PORTABLE_EXTENSION}" >> "${GITHUB_OUTPUT}"
echo "component-dir=${CONFIG_DIR}" >> "${GITHUB_OUTPUT}"

echo "Config resolved from ${CONFIG_FILE}:"
echo "  id:        ${PORTABLE_ID}"
echo "  extension: ${PORTABLE_EXTENSION:-none}"
echo "  dir:       ${CONFIG_DIR}"

# ── Homeboy config dir (project/server/fleet config for operations commands) ──
# Operations commands (deploy/fleet) need a Homeboy project and server, which
# live in Homeboy's config root and are absent on a fresh runner. When the
# caller sets `config-dir`, it names the repo-relative directory that IS the
# config root — it contains projects/, servers/, and optionally components/
# and fleets/ — and this step exports HOMEBOY_CONFIG_ROOT so every later
# homeboy invocation in the run resolves from it (homeboy#14783). Nothing is
# copied and the runner user's real config is never read or touched.

HOMEBOY_CONFIG_DIR_INPUT="${HOMEBOY_CONFIG_DIR_INPUT:-}"
if [ -n "${HOMEBOY_CONFIG_DIR_INPUT}" ]; then
  WORKSPACE_ROOT="${GITHUB_WORKSPACE:-$(pwd)}"
  case "${HOMEBOY_CONFIG_DIR_INPUT}" in
    /*) CONFIG_ROOT="${HOMEBOY_CONFIG_DIR_INPUT}" ;;
    *)  CONFIG_ROOT="${WORKSPACE_ROOT}/${HOMEBOY_CONFIG_DIR_INPUT}" ;;
  esac
  CONFIG_ROOT="${CONFIG_ROOT%/}"

  if [ ! -d "${CONFIG_ROOT}/projects" ]; then
    echo "::error::config-dir '${HOMEBOY_CONFIG_DIR_INPUT}' must contain projects/ (resolved: ${CONFIG_ROOT})"
    echo "::error::Expected layout: <config-dir>/projects/<project-id>/<project-id>.json and <config-dir>/servers/<server-id>.json"
    exit 1
  fi

  PROJECT_IDS="$(find "${CONFIG_ROOT}/projects" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' 2>/dev/null | sort | paste -sd ' ' -)"
  SERVER_IDS="$(find "${CONFIG_ROOT}/servers" -mindepth 1 -maxdepth 1 -type f -name '*.json' -printf '%f\n' 2>/dev/null | sed 's/\.json$//' | sort | paste -sd ' ' -)"

  echo "HOMEBOY_CONFIG_ROOT=${CONFIG_ROOT}" >> "${GITHUB_ENV}"
  echo "homeboy-config-root=${CONFIG_ROOT}" >> "${GITHUB_OUTPUT}"

  echo "Homeboy config root: ${CONFIG_ROOT}"
  echo "  projects: ${PROJECT_IDS:-none}"
  echo "  servers:  ${SERVER_IDS:-none}"
fi
