#!/usr/bin/env bash
set -euo pipefail

# Component env detector for the WordPress extension.
#
# Emits the runtime requirements Homeboy core expects from a
# `component_env.detect_script`:
#
#   {"runtimes":{"php":{"version":"8.4"}}}
#
# Resolution order for the PHP version:
#   1. `extensions.wordpress.settings.wordpress_runtime_php_version` in the
#      component's homeboy.json. This is the version the WP Codebox runtime
#      actually boots, so CI must install the same one or validation
#      dependencies that require a newer PHP silently fail to resolve.
#   2. The `Requires PHP` header of the theme (style.css) or the first plugin
#      main file. This is the component's declared minimum.
#
# An empty object is emitted when nothing is declared so core can fall back
# to extension defaults.

python3 - <<'PY'
import json
import re
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


def homeboy_runtime_php(root):
    config = root / "homeboy.json"
    if not config.exists():
        return None
    try:
        data = json.loads(config.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    settings = (
        data.get("extensions", {})
        .get("wordpress", {})
        .get("settings", {})
    )
    value = settings.get("wordpress_runtime_php_version")
    if isinstance(value, str) and re.fullmatch(r"\d+\.\d+", value.strip()):
        return value.strip()
    return None


root = Path.cwd()
php = homeboy_runtime_php(root)

if php is None:
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
    output["runtimes"] = {"php": {"version": php}}

print(json.dumps(output, separators=(",", ":")))
PY
