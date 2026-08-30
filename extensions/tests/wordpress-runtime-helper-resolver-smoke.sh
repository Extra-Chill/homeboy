#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-runtime-helper-resolver.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

EXTENSIONS_ROOT="${TMP_DIR}/homeboy-extensions"
CORE_ROOT="${TMP_DIR}/homeboy"
HELPER="${CORE_ROOT}/crates/homeboy-core/src/extension/runtime/sidecar-writer.sh"
mkdir -p "$(dirname "$HELPER")" "$EXTENSIONS_ROOT"
touch "$HELPER"
HELPER="$(cd "$(dirname "$HELPER")" && pwd)/sidecar-writer.sh"

resolved="$(HOMEBOY_CORE_DIR="$CORE_ROOT" homeboy_runtime_helper "$EXTENSIONS_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
[ "$resolved" = "$HELPER" ] || { echo "Expected current checkout helper path, got: $resolved" >&2; exit 1; }

resolved="$(unset HOMEBOY_CORE_DIR; homeboy_runtime_helper "$EXTENSIONS_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
[ "$resolved" = "$HELPER" ] || { echo "Expected sibling checkout helper path, got: $resolved" >&2; exit 1; }

OVERRIDE="${TMP_DIR}/override.sh"
touch "$OVERRIDE"
resolved="$(HOMEBOY_RUNTIME_SIDECAR_WRITER="$OVERRIDE" homeboy_runtime_helper "$EXTENSIONS_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
[ "$resolved" = "$OVERRIDE" ] || { echo "Expected explicit override, got: $resolved" >&2; exit 1; }

set +e
diagnostic="$(HOMEBOY_CORE_DIR="${TMP_DIR}/missing" homeboy_runtime_helper "$EXTENSIONS_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh 2>&1)"
status=$?
set -e
[ "$status" -ne 0 ] || { echo "Expected missing helper resolution to fail" >&2; exit 1; }
case "$diagnostic" in
    *"Probed: ${TMP_DIR}/missing/crates/homeboy-core/src/extension/runtime/sidecar-writer.sh"*"Set HOMEBOY_RUNTIME_SIDECAR_WRITER=/path/to/sidecar-writer.sh"*) ;;
    *) echo "Expected actionable missing-helper diagnostic, got: $diagnostic" >&2; exit 1 ;;
esac

echo "WordPress runtime helper resolver smoke passed"
