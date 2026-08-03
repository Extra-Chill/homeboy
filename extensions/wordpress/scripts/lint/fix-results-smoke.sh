#!/usr/bin/env bash
# Smoke-test WordPress fix-result capture using the shared mutation helper.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
FIX_RESULTS_HELPER="${HOMEBOY_RUNTIME_FIX_RESULTS:-${ROOT_DIR}/scripts/lib/fix-results.sh}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi

# shellcheck source=/dev/null
source "$SIDECAR_WRITER_HELPER"
# shellcheck source=../lib/fix-results.sh
source "$FIX_RESULTS_HELPER"

PROJECT_DIR="${TMP_DIR}/project"
RESULTS_FILE="${TMP_DIR}/fix-results.json"
mkdir -p "$PROJECT_DIR"
cat > "${PROJECT_DIR}/plugin.php" <<'PHP'
<?php
echo __( 'before', 'demo' );
PHP

git -C "$PROJECT_DIR" init >/dev/null
git -C "$PROJECT_DIR" add plugin.php
git -C "$PROJECT_DIR" \
    -c user.name="Homeboy Smoke" \
    -c user.email="homeboy-smoke@example.com" \
    commit -m "fixture" >/dev/null

HOMEBOY_FIX_RESULTS_FILE="$RESULTS_FILE"
before_file="$(mktemp)"
homeboy_fix_results_capture "$before_file" "$PROJECT_DIR"
perl -0pi -e 's/before/after/g' "${PROJECT_DIR}/plugin.php"
homeboy_fix_results_append_changed "escape-i18n" "rewrite" "$before_file" "safe" "$PROJECT_DIR"
rm -f "$before_file"
homeboy_fix_results_write

python3 - "$RESULTS_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

expected = [{"file": "plugin.php", "rule": "escape-i18n", "action": "rewrite", "confidence": "safe"}]
if data != expected:
    raise SystemExit(f"unexpected fix results: {data!r} != {expected!r}")
PY

echo "WordPress lint fix-results smoke passed"
