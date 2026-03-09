#!/usr/bin/env bash

set -euo pipefail

source "${GITHUB_ACTION_PATH}/scripts/core/lib.sh"

COMP_ID="$(resolve_component_id)"
WORKSPACE="$(resolve_workspace)"
RESULTS='{}'
OVERALL_EXIT=0
GROUP_PREFIX="${RUN_GROUP_PREFIX:-homeboy}"

HOMEBOY_OUTPUT_DIR=$(mktemp -d)
echo "HOMEBOY_OUTPUT_DIR=${HOMEBOY_OUTPUT_DIR}" >> "${GITHUB_ENV}"

HOMEBOY_ANNOTATIONS_DIR=$(mktemp -d)
echo "HOMEBOY_ANNOTATIONS_DIR=${HOMEBOY_ANNOTATIONS_DIR}" >> "${GITHUB_ENV}"
export HOMEBOY_ANNOTATIONS_DIR

# Enforce canonical order: audit → lint → test
ORDERED_COMMANDS="$(canonicalize_commands "${COMMANDS}")"
IFS=',' read -ra CMD_ARRAY <<< "${ORDERED_COMMANDS}"
HAS_LINT_COMMAND="$(has_lint_command "${COMMANDS}")"

for CMD in "${CMD_ARRAY[@]}"; do
  CMD=$(echo "${CMD}" | xargs)

  # Release is handled by the dedicated release step, not the command loop
  if [ "${CMD}" = "release" ]; then
    continue
  fi

  if [ "${CMD}" = "test" ] && [ "${HAS_LINT_COMMAND}" = "true" ]; then
    export HOMEBOY_SKIP_LINT=1
  else
    unset HOMEBOY_SKIP_LINT 2>/dev/null || true
  fi

  FULL_CMD="$(build_run_command "${CMD}" "${COMP_ID}" "${WORKSPACE}")"

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Running: ${FULL_CMD}"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  echo "::group::${GROUP_PREFIX} ${CMD}"
  CMD_EXIT=0
  set +e
  eval "${FULL_CMD}" 2>&1 | tee "${HOMEBOY_OUTPUT_DIR}/${CMD}.log"
  CMD_EXIT=${PIPESTATUS[0]}
  set -e
  echo "::endgroup::"

  # Extract structured JSON from the log output.
  # Homeboy commands embed JSON objects in their text output. The first
  # top-level JSON object with a "success" key is the output envelope.
  # Extract it into a .json file so downstream scripts can read structured
  # data instead of scraping logs with regex.
  python3 -c "
import json, sys
text = open(sys.argv[1]).read()
# Strip GitHub Actions timestamp prefixes if present
lines = []
for line in text.split('\n'):
    if ' Z ' in line:
        line = line.split(' Z ', 1)[-1]
    lines.append(line)
text = '\n'.join(lines)
decoder = json.JSONDecoder()
for i, c in enumerate(text):
    if c == '{':
        try:
            obj, _ = decoder.raw_decode(text, i)
            if isinstance(obj, dict) and 'success' in obj:
                json.dump(obj, open(sys.argv[2], 'w'), indent=2)
                sys.exit(0)
        except (json.JSONDecodeError, ValueError):
            pass
# No JSON envelope found — write a minimal status-only file
json.dump({'success': sys.argv[3] == '0', 'data': {}}, open(sys.argv[2], 'w'))
" "${HOMEBOY_OUTPUT_DIR}/${CMD}.log" "${HOMEBOY_OUTPUT_DIR}/${CMD}.json" "${CMD_EXIT}" 2>/dev/null || true

  if [ "${CMD_EXIT}" -eq 0 ]; then
    echo "::notice::homeboy ${CMD}: PASSED"
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "pass"}')
  else
    echo "::error::homeboy ${CMD}: FAILED (exit code ${CMD_EXIT})"
    RESULTS=$(echo "${RESULTS}" | jq -c --arg cmd "${CMD}" '. + {($cmd): "fail"}')
    OVERALL_EXIT=1
  fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Results: ${RESULTS}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

echo "results=${RESULTS}" >> "${GITHUB_OUTPUT}"
exit "${OVERALL_EXIT}"
