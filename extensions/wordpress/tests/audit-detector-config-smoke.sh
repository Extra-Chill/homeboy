#!/usr/bin/env bash
set -euo pipefail

EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST_PATH="${1:-${EXTENSION_DIR}/wordpress.json}"
FIXTURE_DIR="${EXTENSION_DIR}/tests/fixtures/audit-detector-config"

python3 - "$MANIFEST_PATH" "$FIXTURE_DIR" <<'PY'
import fnmatch
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

# v0.157.0 best-effort — off-role file suffixes must be exempt from sibling-method conventions.
for pattern in [
    "**/class-*-store.php",
    "**/class-*-registry.php",
    "**/class-*-adopter.php",
    "**/class-*-resolver.php",
    "**/class-*-result.php",
    "**/class-*-diff.php",
    "**/class-*-factory.php",
    "**/class-*-adapter.php",
    "**/class-*-lock.php",
]:
    require(pattern in exception_globs, f"missing PHP role exception glob: {pattern}")

lifecycle_globs = set(rules.get("lifecycle_path_globs", []))
for pattern in ["**/*-smoke.php", "**/*-fallback.php", "**/*-shim.php", "**/*-stub.php"]:
    require(pattern in lifecycle_globs, f"missing contextual dead-guard path glob: {pattern}")

# Forward-compat (core main): convention_tag_globs splits unrelated PHP roles.
tag_globs = rules.get("convention_tag_globs", [])
require(tag_globs, "convention_tag_globs must be configured to split PHP roles in mixed directories")

required_tags = {
    "wordpress:php-role:procedural-helper",
    "wordpress:php-role:contract",
    "wordpress:php-role:registry",
    "wordpress:php-role:result",
    "wordpress:php-role:adapter",
    "wordpress:php-role:factory",
    "wordpress:php-role:lock",
    "wordpress:php-role:null-impl",
}
have_tags = {entry["tag"] for entry in tag_globs}
missing_tags = required_tags - have_tags
require(not missing_tags, f"missing convention_tag_globs roles: {sorted(missing_tags)}")

# Tags must be opaque, namespaced strings — core never interprets them.
for entry in tag_globs:
    require("tag" in entry and "globs" in entry, f"convention_tag_globs entry malformed: {entry}")
    require(entry["tag"].startswith("wordpress:"), f"convention tag must be namespaced: {entry['tag']}")
    require(entry["globs"], f"convention tag {entry['tag']} must declare path globs")

# Each role's globs must match files in the canonical role.
def globs_for(tag):
    for entry in tag_globs:
        if entry["tag"] == tag:
            return entry["globs"]
    return []

def matches_any(path, patterns):
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)

# Identity fixture: store contract gets contract tag; value objects get no tag.
identity_store = "src/Identity/class-demo-identity-store.php"
identity_scope = "src/Identity/class-demo-identity-scope.php"
identity_materialized = "src/Identity/class-demo-materialized-identity.php"
require(matches_any(identity_store, globs_for("wordpress:php-role:contract")),
        "identity store fixture must be tagged as contract role")
require(not matches_any(identity_scope, globs_for("wordpress:php-role:contract")),
        "identity scope value object must not be tagged as contract role")
require(not matches_any(identity_materialized, globs_for("wordpress:php-role:contract")),
        "materialized identity value object must not be tagged as contract role")

# Packages fixture: split into value/contract/result/registry/procedural roles.
packages_value_paths = [
    "src/Packages/class-demo-package.php",
    "src/Packages/class-demo-package-artifact.php",
    "src/Packages/class-demo-package-artifact-type.php",
]
for path in packages_value_paths:
    for tag in required_tags:
        require(not matches_any(path, globs_for(tag)),
                f"value-object fixture {path} must remain untagged (matched {tag})")

require(matches_any("src/Packages/class-demo-package-adopter.php", globs_for("wordpress:php-role:contract")),
        "adopter interface fixture must be tagged as contract role")
require(matches_any("src/Packages/class-demo-package-adoption-result.php", globs_for("wordpress:php-role:result")),
        "adoption result fixture must be tagged as result role")
require(matches_any("src/Packages/class-demo-package-adoption-diff.php", globs_for("wordpress:php-role:result")),
        "adoption diff fixture must be tagged as result role")
require(matches_any("src/Packages/class-demo-package-artifacts-registry.php", globs_for("wordpress:php-role:registry")),
        "artifacts registry fixture must be tagged as registry role")
require(matches_any("src/Packages/register-demo-package-artifacts.php", globs_for("wordpress:php-role:procedural-helper")),
        "procedural helper fixture must be tagged as procedural-helper role")

# v0.157.0 best-effort fallback: every off-role file must also be in convention_exception_globs.
for path in [
    "src/Identity/class-demo-identity-store.php",
    "src/Packages/class-demo-package-adopter.php",
    "src/Packages/class-demo-package-adoption-result.php",
    "src/Packages/class-demo-package-adoption-diff.php",
    "src/Packages/class-demo-package-artifacts-registry.php",
    "src/Packages/register-demo-package-artifacts.php",
]:
    require(matches_any(path, exception_globs),
            f"off-role file {path} must also be exempt for v0.157.0 fallback")

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
    "src/Identity/class-demo-identity-scope.php",
    "src/Identity/class-demo-identity-store.php",
    "src/Identity/class-demo-materialized-identity.php",
    "src/Packages/class-demo-package.php",
    "src/Packages/class-demo-package-artifact.php",
    "src/Packages/class-demo-package-artifact-type.php",
    "src/Packages/class-demo-package-adopter.php",
    "src/Packages/class-demo-package-adoption-result.php",
    "src/Packages/class-demo-package-adoption-diff.php",
    "src/Packages/class-demo-package-artifacts-registry.php",
    "src/Packages/register-demo-package-artifacts.php",
]:
    require((fixture_dir / relative).exists(), f"missing audit detector fixture: {relative}")

print("wordpress audit detector config smoke passed")
PY
