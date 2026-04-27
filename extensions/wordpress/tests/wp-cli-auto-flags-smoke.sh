#!/usr/bin/env bash
set -euo pipefail

MANIFEST_PATH="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/wordpress.json}"

python3 - "$MANIFEST_PATH" <<'PY'
import json
import sys

manifest_path = sys.argv[1]
with open(manifest_path, encoding="utf-8") as handle:
    manifest = json.load(handle)

auto_flags = manifest.get("cli", {}).get("auto_flags", [])
expected = {
    "when": {"server_user": "root"},
    "flag": "--allow-root",
}

if expected not in auto_flags:
    raise SystemExit("wordpress cli.auto_flags must declare root => --allow-root")

print("wordpress cli auto_flags smoke passed")
PY
