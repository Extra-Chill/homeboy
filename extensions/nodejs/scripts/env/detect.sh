#!/usr/bin/env bash
set -euo pipefail

# Component env detector for the Node.js extension.
#
# Emits the runtime requirements Homeboy core expects from a
# `component_env.detect_script`, run from the component directory:
#
#   {"runtimes":{"node":{"version":"22"}}}
#
# Without this, core never learns that a Node component needs Node, so CI
# skips Node setup entirely and anything the component invokes — its declared
# package manager included — is missing.
#
# Resolution order for the Node version:
#   1. `.nvmrc`, then `.node-version`: an explicit pin wins.
#   2. `engines.node` in package.json, resolved to the lowest major version
#      the range admits (">=20" and "^20.11" both yield "20"). This is the
#      component's declared floor, the same way WordPress uses `Requires PHP`.
#
# A value that is not a plain version or a range with a readable major is
# ignored rather than guessed. An empty object is emitted when nothing is
# declared so core can fall back to extension defaults.

python3 - <<'PY'
import json
import re
from pathlib import Path

VERSION = re.compile(r"^v?(\d+)(?:\.\d+){0,2}$")


def pinned_version(root):
    for name in (".nvmrc", ".node-version"):
        path = root / name
        if not path.is_file():
            continue
        try:
            value = path.read_text(encoding="utf-8").strip()
        except OSError:
            continue
        match = VERSION.match(value)
        if match:
            return value.lstrip("v")
    return None


def engines_floor(root):
    manifest = root / "package.json"
    if not manifest.is_file():
        return None
    try:
        engines = json.loads(manifest.read_text(encoding="utf-8")).get("engines") or {}
    except (OSError, ValueError, AttributeError):
        return None
    spec = engines.get("node") if isinstance(engines, dict) else None
    if not isinstance(spec, str):
        return None
    # For each "||" alternative, take the first version it names; the lowest
    # major across alternatives is the floor. Upper bounds such as "<21" are
    # never floors, so an alternative that only states one is skipped.
    majors = []
    for alternative in spec.split("||"):
        for comparator in alternative.split():
            if comparator.startswith("<"):
                continue
            match = re.search(r"(\d+)", comparator)
            if match:
                majors.append(int(match.group(1)))
                break
    return str(min(majors)) if majors else None


root = Path.cwd()
node = pinned_version(root) or engines_floor(root)

output = {}
if node:
    output["runtimes"] = {"node": {"version": node}}

print(json.dumps(output, separators=(",", ":")))
PY
