#!/usr/bin/env bash
set -euo pipefail

python3 - <<'PY'
import json
from pathlib import Path


def header_value(path, key):
    try:
        with path.open("r", encoding="utf-8", errors="ignore") as handle:
            for index, line in enumerate(handle):
                if index >= 100:
                    break
                marker = key + ":"
                if marker in line:
                    value = line.split(marker, 1)[1].strip()
                    if value:
                        return value
    except OSError:
        return None
    return None


root = Path.cwd()
php = None

style = root / "style.css"
if style.exists() and header_value(style, "Theme Name"):
    php = header_value(style, "Requires PHP")

if php is None:
    for candidate in sorted(root.glob("*.php")):
        if not header_value(candidate, "Plugin Name"):
            continue
        php = header_value(candidate, "Requires PHP")
        break

output = {}
if php:
    output["php"] = php

print(json.dumps(output, separators=(",", ":")))
PY
