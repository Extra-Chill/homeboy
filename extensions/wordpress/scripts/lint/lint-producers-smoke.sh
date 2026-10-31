#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi

COMPONENT_DIR="${TMP_DIR}/component"
FINDINGS_FILE="${TMP_DIR}/lint-findings.json"
PRODUCERS_FILE="${TMP_DIR}/lint-producers.json"
OUTPUT_FILE="${TMP_DIR}/lint-output.txt"
mkdir -p "$COMPONENT_DIR"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Lint Producers Fixture
 * Text Domain: lint-producers-fixture
 */
PHP

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="lint-producers-fixture" \
HOMEBOY_STEP="eslint" \
HOMEBOY_LINT_FINDINGS_FILE="$FINDINGS_FILE" \
HOMEBOY_LINT_PRODUCERS_FILE="$PRODUCERS_FILE" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    bash "${SCRIPT_DIR}/lint-runner.sh" > "$OUTPUT_FILE" 2>&1

python3 - "$PRODUCERS_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    producers = json.load(handle)

expected_tools = {"phpcs", "eslint", "phpstan"}
actual_tools = {producer.get("tool") for producer in producers}
if actual_tools != expected_tools:
    raise SystemExit(f"unexpected producer tools: {producers!r}")

for producer in producers:
    for key in ("tool", "status", "finding_count", "step"):
        if key not in producer:
            raise SystemExit(f"missing {key} in producer summary: {producer!r}")
    if producer["finding_count"] != 0:
        raise SystemExit(f"expected zero-finding producer summary: {producer!r}")
PY

echo "WordPress lint producers smoke passed"
