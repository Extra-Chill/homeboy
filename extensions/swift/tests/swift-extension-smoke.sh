#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST="$ROOT_DIR/swift/swift.json"

python3 - "$MANIFEST" <<'PY'
import json
import sys

manifest_path = sys.argv[1]
with open(manifest_path, encoding="utf-8") as handle:
    manifest = json.load(handle)

def assert_true(condition, message):
    if not condition:
        raise AssertionError(message)

provides = manifest.get("provides", {})
extensions = provides.get("file_extensions", [])
for ext in ["swift", "xcodeproj", "xcworkspace", "yml"]:
    assert_true(ext in extensions, f"missing file extension: {ext}")

discovery = manifest.get("platform", {}).get("discovery", {})
find_command = discovery.get("find_command", "")
assert_true("project.yml" in find_command, "discovery should include project.yml")
assert_true("*.xcodeproj" in find_command, "discovery should include xcodeproj")
assert_true("*.xcworkspace" in find_command, "discovery should include xcworkspace")
assert_true("*.swift" in find_command, "discovery should include Swift sources")
assert_true(discovery.get("base_path_transform") == "identity", "unexpected base path transform")

audit = manifest.get("audit", {})
patterns = audit.get("feature_patterns", [])
for pattern in ["class\\s+(\\w+)", "struct\\s+(\\w+)", "enum\\s+(\\w+)", "protocol\\s+(\\w+)", "func\\s+(\\w+)\\s*\\("]:
    assert_true(pattern in patterns, f"missing feature pattern: {pattern}")

labels = audit.get("feature_labels", {})
for label in ["Classes", "Structs", "Enums", "Protocols", "Functions"]:
    assert_true(label in labels.values(), f"missing feature label: {label}")

mapping = audit.get("test_mapping", {})
assert_true("tests" in mapping.get("test_dirs", []), "missing lowercase tests dir")
assert_true("Tests" in mapping.get("test_dirs", []), "missing uppercase Tests dir")
assert_true(mapping.get("test_file_pattern") == "{dir}/{name}Tests.{ext}", "unexpected test file pattern")

for key in ["lint", "scripts"]:
    assert_true(key not in manifest, f"#{key} belongs to a later PR")

print("swift manifest smoke passed")
PY
