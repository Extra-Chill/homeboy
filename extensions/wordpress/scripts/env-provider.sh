#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER_SOURCE="${SCRIPT_DIR}/runtime/bench-helper.php"

python3 - "$HELPER_SOURCE" <<'PY'
import json
import sys

helper_source = sys.argv[1]
print(json.dumps({
    "HOMEBOY_REDACTION_SENSITIVE_HEADERS": "x-wp-nonce",
    "HOMEBOY_RUNTIME_HELPERS_JSON": json.dumps([
        {
            "filename": "bench-helper.php",
            "source": helper_source,
            "env_var": "HOMEBOY_RUNTIME_BENCH_HELPER_PHP",
            "legacy_fallback": True,
        }
    ], separators=(",", ":")),
}, separators=(",", ":")))
PY
