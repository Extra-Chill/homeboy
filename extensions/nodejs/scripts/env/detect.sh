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
#   2. `engines.node` in package.json, resolved to the newest supported Node
#      LTS major the range admits. A range states what the component runs on,
#      not what to install: ">=14" admits the current LTS, so it yields
#      CURRENT_LTS rather than an end-of-life floor whose npm cannot read
#      today's lockfiles. A range that admits nothing up to CURRENT_LTS yields
#      its own floor (">=26" yields "26"); one capped below it yields the
#      newest LTS major under the cap (">=18 <23" yields "22", "^20" yields
#      "20").
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


# The newest Node LTS major. Node LTS lines are even majors.
CURRENT_LTS = 24
INFINITY = float("inf")


def alternative_bounds(alternative):
    """Inclusive (lowest, highest) major one `||` alternative admits."""
    lowest, highest = 0, INFINITY
    # A hyphen range "16 - 20" is ">=16 <=20".
    hyphen = re.match(r"^\s*(\S+)\s+-\s+(\S+)\s*$", alternative)
    tokens = [">=" + hyphen.group(1), "<=" + hyphen.group(2)] if hyphen else alternative.split()
    for token in tokens:
        match = re.match(r"^(\^|~|>=|>|<=|<|=)?v?(\d+)((?:\.(?:\d+|x|\*))*)$", token)
        if not match:
            continue
        operator, major, rest = match.group(1) or "", int(match.group(2)), match.group(3)
        exact_major = not re.search(r"[1-9]", rest)
        if operator in (">=", ">"):
            lowest = max(lowest, major)
        elif operator == "<":
            highest = min(highest, major - 1 if exact_major else major)
        elif operator == "<=":
            highest = min(highest, major)
        else:
            lowest, highest = max(lowest, major), min(highest, major)
    return lowest, highest


def engines_version(root):
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
    candidates = []
    for alternative in spec.split("||"):
        if not re.search(r"\d", alternative):
            continue
        lowest, highest = alternative_bounds(alternative)
        if lowest == 0 and highest == INFINITY:
            continue
        if lowest > highest:
            continue
        if lowest >= CURRENT_LTS:
            candidates.append(lowest)
            continue
        newest = int(min(highest, CURRENT_LTS))
        if newest % 2 and newest - 1 >= lowest:
            newest -= 1
        if newest >= lowest:
            candidates.append(newest)
    return str(max(candidates)) if candidates else None


root = Path.cwd()
node = pinned_version(root) or engines_version(root)

output = {}
if node:
    output["runtimes"] = {"node": {"version": node}}

print(json.dumps(output, separators=(",", ":")))
PY
