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
command_template = manifest.get("cli", {}).get("command_template", "")
expected = {
    "when": {"server_user": "root"},
    "flag": "--allow-root",
}

if expected not in auto_flags:
    raise SystemExit("wordpress cli.auto_flags must declare root => --allow-root")

args_index = command_template.find("{{args}}")
for global_arg in ("--path={{sitePath}}", "--url={{domain}}"):
    global_index = command_template.find(global_arg)
    if global_index == -1:
        raise SystemExit(f"wordpress cli.command_template must include {global_arg}")
    if args_index == -1 or global_index > args_index:
        raise SystemExit(
            f"wordpress cli.command_template must place {global_arg} before {{args}}"
        )

print("wordpress cli auto_flags smoke passed")
PY
