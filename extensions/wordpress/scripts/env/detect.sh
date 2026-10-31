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
#      main file (else composer.json), raised to any already-present local
#      validation dependency's floor. PHPCS still uses the component floor.
#
# An empty object is emitted when nothing is declared so core can fall back
# to extension defaults.

# Dependencies may not be fetched yet. Inspect explicit local paths only: env
# detection must not clone or fail on unresolved slugs. PHPStan checks the full
# materialized graph later, including dependencies unavailable at detect time.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 - "${SCRIPT_DIR}/../lib" <<'PY'
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, sys.argv[1])
from php_floor import declared_floor, local_dependency_paths, version


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
    php = declared_floor(root)
    for dependency in local_dependency_paths(root):
        floor = declared_floor(dependency)
        if floor and (not php or version(floor) > version(php)):
            php = floor

output = {}
if php:
    output["runtimes"] = {"php": {"version": php}}

print(json.dumps(output, separators=(",", ":")))
PY
