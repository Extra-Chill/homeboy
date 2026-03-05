#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/lib.sh"

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"

echo "Registering component: ${COMP_ID}"

CREATE_CMD="homeboy component create --local-path ${WORKSPACE}"
if [ -n "${EXTENSION_ID:-}" ]; then
  CREATE_CMD="${CREATE_CMD} --extension ${EXTENSION_ID}"
fi

echo "Running: ${CREATE_CMD}"
eval "${CREATE_CMD}"

if [ -n "${SETTINGS_JSON:-}" ] && [ "${SETTINGS_JSON}" != "{}" ] && [ -n "${EXTENSION_ID:-}" ]; then
  echo "Applying extension settings..."
  NESTED_JSON=$(echo "${SETTINGS_JSON}" | jq --arg ext "${EXTENSION_ID}" '{extensions: {($ext): {settings: .}}}')
  homeboy component set "${COMP_ID}" --json "${NESTED_JSON}"
fi

homeboy component list 2>/dev/null | grep -q "${COMP_ID}" || {
  echo "::error::Failed to register component '${COMP_ID}'"
  exit 1
}

echo "Component '${COMP_ID}' registered successfully"
echo "component_id=${COMP_ID}" >> "${GITHUB_ENV}"
