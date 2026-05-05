#!/usr/bin/env bash
set -euo pipefail

EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST_PATH="${1:-${EXTENSION_DIR}/wordpress.json}"
FIXTURE_DIR="${EXTENSION_DIR}/tests/fixtures/audit-detector-config"

python3 - "$MANIFEST_PATH" "$FIXTURE_DIR" <<'PY'
import json
import re
import sys
from pathlib import Path

manifest_path = Path(sys.argv[1])
fixture_dir = Path(sys.argv[2])
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
rules = manifest["audit"]["detector_rules"]

def require(condition, message):
    if not condition:
        raise SystemExit(message)

utility_suffixes = set(rules.get("utility_suffixes", []))
for suffix in ["Contract", "Interface", "Store", "Lock", "Result", "Value", "Package"]:
    require(suffix in utility_suffixes, f"missing PHP role utility suffix: {suffix}")

exception_globs = set(rules.get("convention_exception_globs", []))
require("**/register-*.php" in exception_globs, "procedural register helpers must be convention-exempt")
require("**/*-functions.php" in exception_globs, "procedural functions files must be convention-exempt")

lifecycle_globs = set(rules.get("lifecycle_path_globs", []))
for pattern in ["**/*-smoke.php", "**/*-fallback.php", "**/*-shim.php", "**/*-stub.php"]:
    require(pattern in lifecycle_globs, f"missing contextual dead-guard path glob: {pattern}")

literal_rule = next(
    rule for rule in rules["requested_detectors"] if rule["id"] == "wordpress-constant-backed-slug-literal"
)
literal_pattern = literal_rule["literal_pattern"].replace("{value}", re.escape("tool_call"))
literal_regex = re.compile(literal_pattern, re.MULTILINE)
fixture = (fixture_dir / "src/Runtime/class-demo-event.php").read_text(encoding="utf-8")
matches = [match.group(0) for match in literal_regex.finditer(fixture)]

require(any("===" in match for match in matches), "constant-backed slug detector should still flag comparisons")
require(not any(match.strip() == "'tool_call'" for match in matches), "detector must not match a bare literal everywhere")
require("'tool_call' =>" in fixture, "fixture should include array/protocol key literal")
require("do_action( 'tool_call'" in fixture, "fixture should include event-name literal")

for relative in [
    "src/Registry/register-agents.php",
    "src/Contracts/class-demo-message-store.php",
    "src/Runtime/class-demo-message-result.php",
    "tests/runtime-smoke.php",
]:
    require((fixture_dir / relative).exists(), f"missing audit detector fixture: {relative}")

print("wordpress audit detector config smoke passed")
PY
