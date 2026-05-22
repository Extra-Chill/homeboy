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
audit = manifest["audit"]

def require(condition, message):
    if not condition:
        raise SystemExit(message)

def matches_any(path, patterns):
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)

utility_suffixes = set(rules.get("utility_suffixes", []))
for suffix in ["Authenticator", "Base", "Constants", "Contract", "Credential", "Handlers", "Interface", "Projector", "Sanitizer", "Scheduler", "Store", "Lock", "Policy", "Result", "Secret", "Service", "Token", "Value", "Package", "Verifier"]:
    require(suffix in utility_suffixes, f"missing PHP role utility suffix: {suffix}")

exception_globs = set(rules.get("convention_exception_globs", []))
require("**/register.php" in exception_globs, "procedural register.php loaders must be convention-exempt")
require("**/register-*.php" in exception_globs, "procedural register helpers must be convention-exempt")
require("**/*-functions.php" in exception_globs, "procedural functions files must be convention-exempt")

ignore_claim_patterns = audit.get("ignore_claim_patterns", [])
for path in [
    "daily/YYYY/MM/DD.md",
    "wp-content/plugins/<slug>/README.md",
    "records/{id}/index.md",
]:
    require(matches_any(path, ignore_claim_patterns),
            f"placeholder doc reference must be ignored by docs audit: {path}")
require(not matches_any("docs/missing-real-file.md", ignore_claim_patterns),
        "real broken doc references must not be hidden by placeholder ignore patterns")

duplication_detector = rules.get("duplication_detector", {})
for call in ["array", "empty", "get_param"]:
    require(call in set(duplication_detector.get("trivial_calls", [])),
            f"generic PHP/REST call must be trivial in parallel-implementation signal: {call}")
for call in ["WP_REST_Response", "rest_ensure_response"]:
    require(call in set(duplication_detector.get("plumbing_calls", [])),
            f"WordPress REST response wrapper must be plumbing in parallel-implementation signal: {call}")
for call in ["wp_get_ability", "is_wp_error", "get_error_message"]:
    require(call in set(duplication_detector.get("plumbing_calls", [])),
            f"WordPress ability CLI boilerplate must be plumbing in parallel-implementation signal: {call}")

# v0.157.0 best-effort — off-role file suffixes must be exempt from sibling-method conventions.
for pattern in [
    "**/class-*-store.php",
    "**/class-*-registry.php",
    "**/class-*-adopter.php",
    "**/class-*-resolver.php",
    "**/class-*-sanitizer.php",
    "**/class-*-authenticator.php",
    "**/class-*-service.php",
    "**/class-*-scheduler.php",
    "**/class-*-policy.php",
    "**/class-*-config.php",
    "**/class-*-constants.php",
    "**/class-*-token.php",
    "**/class-*-credential.php",
    "**/class-*-result.php",
    "**/class-*-diff.php",
    "**/class-*-factory.php",
    "**/class-*-adapter.php",
    "**/class-*-lock.php",
    "**/class-*-artifact.php",
    "**/class-*-artifacts.php",
    "**/*Constants.php",
    "**/*Resolver.php",
    "**/*Result.php",
    "**/*Sanitizer.php",
    "**/*Scheduler.php",
    "**/*Verifier.php",
]:
    require(pattern in exception_globs, f"missing PHP role exception glob: {pattern}")

lifecycle_globs = set(rules.get("lifecycle_path_globs", []))
for pattern in ["**/*-smoke.php", "**/*-fallback.php", "**/*-shim.php", "**/*-stub.php"]:
    require(pattern in lifecycle_globs, f"missing contextual dead-guard path glob: {pattern}")

# WP-CLI command-output policy recognizers live in the WordPress extension, not
# homeboy core. Core consumes these as opaque substring rules.
framework_command_recognizers = rules.get("framework_command_recognizers", [])
recognizers_by_id = {entry["id"]: entry for entry in framework_command_recognizers}
for recognizer_id in [
    "wp-cli-direct-registration",
    "wp-cli-direct-subclass",
    "wp-cli-indirect-subclass",
]:
    require(recognizer_id in recognizers_by_id,
            f"missing WP-CLI command recognizer: {recognizer_id}")

direct_registration = recognizers_by_id["wp-cli-direct-registration"]
require("WP_CLI::add_command" in direct_registration.get("requires_all", []),
        "direct registration recognizer must require WP_CLI::add_command")

direct_subclass = recognizers_by_id["wp-cli-direct-subclass"]
require("extends \\WP_CLI_Command" in direct_subclass.get("requires_any", []),
        "direct subclass recognizer must accept fully-qualified WP_CLI_Command")

indirect_subclass = recognizers_by_id["wp-cli-indirect-subclass"]
any_groups = indirect_subclass.get("requires_any_groups", [])
require(len(any_groups) == 2,
        "indirect subclass recognizer must require both an import group and an output-method group")
require(any("use WP_CLI;" in group for group in any_groups),
        "indirect subclass recognizer must require a WP_CLI import marker")
require(any("WP_CLI::success" in group and "WP_CLI::error" in group for group in any_groups),
        "indirect subclass recognizer must require a WP_CLI output-method marker")

# Forward-compat (core main): convention_tag_globs splits unrelated PHP roles.
tag_globs = rules.get("convention_tag_globs", [])
require(tag_globs, "convention_tag_globs must be configured to split PHP roles in mixed directories")

required_tags = {
    "wordpress:php-role:procedural-helper",
    "wordpress:php-role:contract",
    "wordpress:php-role:registry",
    "wordpress:php-role:result",
    "wordpress:php-role:artifact",
    "wordpress:php-role:adapter",
    "wordpress:php-role:service",
    "wordpress:php-role:factory",
    "wordpress:php-role:configuration",
    "wordpress:php-role:credential",
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

# Packages fixture: split into value/contract/result/artifact/registry/procedural roles.
# Manifest + artifact-type are sibling value objects (slug/type + args ctor shape) and
# stay untagged so they discover a shared convention. Artifact declaration objects are
# their own role — different ctor shape (single declaration array) — and must be tagged
# so core does not pollute the manifest convention with artifact-declaration constructors.
packages_value_paths = [
    "src/Packages/class-demo-package.php",
    "src/Packages/class-demo-package-artifact-type.php",
]
for path in packages_value_paths:
    for tag in required_tags:
        require(not matches_any(path, globs_for(tag)),
                f"value-object fixture {path} must remain untagged (matched {tag})")

require(matches_any("src/Packages/class-demo-package-adopter.php", globs_for("wordpress:php-role:contract")),
        "adopter interface fixture must be tagged as contract role")
require(matches_any("src/Packages/class-demo-package-artifact.php", globs_for("wordpress:php-role:artifact")),
        "artifact declaration fixture must be tagged as artifact role")
# Boundary check: `*-artifact-type.php` must NOT match the artifact role glob.
require(not matches_any("src/Packages/class-demo-package-artifact-type.php", globs_for("wordpress:php-role:artifact")),
        "artifact-type registration fixture must not be tagged as artifact role")
require(matches_any("src/Packages/class-demo-package-adoption-result.php", globs_for("wordpress:php-role:result")),
        "adoption result fixture must be tagged as result role")
require(matches_any("src/Packages/class-demo-package-adoption-diff.php", globs_for("wordpress:php-role:result")),
        "adoption diff fixture must be tagged as result role")
require(matches_any("src/Packages/class-demo-package-artifacts-registry.php", globs_for("wordpress:php-role:registry")),
        "artifacts registry fixture must be tagged as registry role")
require(matches_any("src/Packages/register-demo-package-artifacts.php", globs_for("wordpress:php-role:procedural-helper")),
        "procedural helper fixture must be tagged as procedural-helper role")
require(matches_any("src/Packages/register.php", globs_for("wordpress:php-role:procedural-helper")),
        "register.php loader fixture must be tagged as procedural-helper role")

# Auth fixture: normal DTOs stay in the value-object convention; credential/token
# objects and request-edge authenticators get separate role tags so neither
# inherits full DTO serialization methods from the other.
auth_value_paths = [
    "src/Auth/class-demo-access-grant.php",
]
for path in auth_value_paths:
    for tag in required_tags:
        require(not matches_any(path, globs_for(tag)),
                f"auth value-object fixture {path} must remain untagged (matched {tag})")

require(matches_any("src/Auth/class-demo-token-authenticator.php", globs_for("wordpress:php-role:service")),
        "authenticator fixture must be tagged as service role")
require(not matches_any("src/Auth/class-demo-token-authenticator.php", globs_for("wordpress:php-role:contract")),
        "authenticator fixture must not be tagged as contract role")
require(matches_any("src/Auth/class-demo-token.php", globs_for("wordpress:php-role:credential")),
        "token fixture must be tagged as credential role")
require(not matches_any("src/Auth/class-demo-token.php", globs_for("wordpress:php-role:service")),
        "token fixture must not be tagged as service role")

# Context fixture: resolver interfaces remain contract role files; policy/config
# vocabulary classes are configuration role files, not resolver contracts.
require(matches_any("src/Context/class-demo-context-conflict-resolver.php", globs_for("wordpress:php-role:contract")),
        "context resolver fixture must be tagged as contract role")
require(matches_any("src/Context/class-demo-context-injection-policy.php", globs_for("wordpress:php-role:configuration")),
        "context injection policy fixture must be tagged as configuration role")
require(not matches_any("src/Context/class-demo-context-injection-policy.php", globs_for("wordpress:php-role:contract")),
        "context injection policy fixture must not be tagged as contract role")

# PSR-4 API fixture: helper/value/resolver classes can live beside real REST
# route/controller classes under broad Api namespaces. Only the helper roles are
# exempt/tagged; route-like classes remain in the normal convention group so
# missing register()/register_routes()/rest_api_init signals still report there.
api_helper_roles = {
    "src/Api/WebhookAuthResolver.php": "wordpress:php-role:contract",
    "src/Api/WebhookVerificationResult.php": "wordpress:php-role:result",
    "src/Api/WebhookVerifier.php": "wordpress:php-role:service",
}
for path, tag in api_helper_roles.items():
    require(matches_any(path, exception_globs),
            f"PSR-4 API helper {path} must be exempt from registrar/controller conventions")
    require(matches_any(path, globs_for(tag)),
            f"PSR-4 API helper {path} must be tagged as {tag}")

for path in [
    "src/Api/WebhookRoutes.php",
    "src/Api/WebhookController.php",
    "src/Api/WebhookStatusController.php",
]:
    require(not matches_any(path, exception_globs),
            f"route/controller fixture {path} must not be exempt from registration audits")
    for tag in required_tags:
        require(not matches_any(path, globs_for(tag)),
                f"route/controller fixture {path} must stay in normal convention group (matched {tag})")

# Broad Abilities directories can contain support classes. Ability-like classes
# stay in the untagged Ability convention; helper/service/configuration classes
# get split out so they are not forced to carry an Ability suffix.
for path in [
    "src/Abilities/CreateAbility.php",
    "src/Abilities/UpdateAbility.php",
    "src/Abilities/FlowThing.php",
]:
    for tag in required_tags:
        require(not matches_any(path, globs_for(tag)),
                f"ability convention fixture {path} must remain untagged (matched {tag})")

require(matches_any("src/Abilities/FileConstants.php", globs_for("wordpress:php-role:configuration")),
        "ability constants fixture must be tagged as configuration role")
require(matches_any("src/Abilities/BlockSanitizer.php", globs_for("wordpress:php-role:service")),
        "ability sanitizer fixture must be tagged as service role")
require(matches_any("src/Abilities/PipelineBatchScheduler.php", globs_for("wordpress:php-role:service")),
        "ability scheduler fixture must be tagged as service role")

# v0.157.0 best-effort fallback: every off-role file must also be in convention_exception_globs.
for path in [
    "src/Identity/class-demo-identity-store.php",
    "src/Packages/class-demo-package-adopter.php",
    "src/Packages/class-demo-package-artifact.php",
    "src/Packages/class-demo-package-adoption-result.php",
    "src/Packages/class-demo-package-adoption-diff.php",
    "src/Packages/class-demo-package-artifacts-registry.php",
    "src/Packages/register-demo-package-artifacts.php",
    "src/Auth/class-demo-token.php",
    "src/Auth/class-demo-token-authenticator.php",
    "src/Context/class-demo-context-conflict-resolver.php",
    "src/Context/class-demo-context-injection-policy.php",
    "src/Api/WebhookAuthResolver.php",
    "src/Api/WebhookVerificationResult.php",
    "src/Api/WebhookVerifier.php",
    "src/Abilities/FileConstants.php",
    "src/Abilities/BlockSanitizer.php",
    "src/Abilities/PipelineBatchScheduler.php",
]:
    require(matches_any(path, exception_globs),
            f"off-role file {path} must also be exempt for v0.157.0 fallback")

# Option scope drift detector — must require explicit drift vocabulary in a
# comment block (claims of network/site-option storage), recognize file-level
# opt-out markers, and ignore incidental mentions of "multisite"/"network".
# Regression coverage for Extra-Chill/homeboy-extensions#424.
option_scope_rule = next(
    rule for rule in rules["requested_detectors"] if rule["id"] == "wordpress-option-scope-drift"
)
include_regex = re.compile(option_scope_rule["comment_pattern"])
exclude_regex = re.compile(option_scope_rule["comment_exclude_pattern"])
call_regex = re.compile(option_scope_rule["pattern"])

drift_fixture = (fixture_dir / "src/Options/class-demo-network-drift.php").read_text(encoding="utf-8")
single_site_fixture = (fixture_dir / "src/Options/class-demo-single-site-noise.php").read_text(encoding="utf-8")
opt_out_fixture = (fixture_dir / "src/Options/class-demo-opt-out-marker.php").read_text(encoding="utf-8")

# True-positive fixture: drift docblock vocabulary triggers include, no
# opt-out, and three option call sites are flagged.
require(include_regex.search(drift_fixture),
        "drift fixture must trigger option-scope include pattern")
require(not exclude_regex.search(drift_fixture),
        "drift fixture must not trigger option-scope exclude pattern")
drift_calls = call_regex.findall(drift_fixture)
require(len(drift_calls) == 3,
        f"drift fixture should expose 3 option call sites, got {len(drift_calls)}")

# False-positive fixture: incidental "multisite"/"network" mentions must NOT
# trigger the include pattern — this is the over-fire that issue #424 fixed.
require(not include_regex.search(single_site_fixture),
        "single-site noise fixture must not trigger option-scope include pattern (regression for #424)")
# Sanity: the file does mention the loose vocabulary, just not the tight one.
require(re.search(r"(?i)\bmultisite\b", single_site_fixture),
        "single-site fixture must contain bare 'multisite' to exercise the previous over-fire shape")
require(re.search(r"(?i)\bnetwork\s+request\b", single_site_fixture),
        "single-site fixture must contain a non-storage 'network' mention")

# Opt-out fixture: even when drift vocabulary appears, the file-level
# `@option-scope single-site` marker suppresses the detector for the whole
# file.
require(include_regex.search(opt_out_fixture),
        "opt-out fixture should contain drift vocabulary to exercise the suppress path")
require(exclude_regex.search(opt_out_fixture),
        "opt-out fixture must trigger the file-level @option-scope single-site exclude marker")

# Exclude pattern must recognize each documented opt-out phrase.
for phrase in [
    "@option-scope single-site",
    "@option-scope: single-site",
    "single-site option",
    "single-site only",
    "single-site plugin",
    "not a network option",
    "does not support multisite",
    "do not support multisite",
    "no multisite support",
    "multisite: false",
]:
    require(exclude_regex.search(phrase),
            f"option-scope exclude pattern must recognize opt-out phrase: {phrase!r}")

# Include pattern must NOT match incidental vocabulary that issue #424
# called out as the false-positive shape.
for phrase in [
    "network request",
    "multisite-aware logger",
    "multisite",
    "network",
    "network option",  # bare phrase no longer enough on its own
    "site option",     # bare phrase no longer enough on its own
]:
    require(not include_regex.search(phrase),
            f"option-scope include pattern must not fire on incidental phrase: {phrase!r}")

# Include pattern MUST match explicit drift vocabulary.
for phrase in [
    "stored as a network option",
    "stored as a site option",
    "should use update_site_option",
    "should use site option API",
    "must use get_site_option",
    "network-wide setting",
    "network wide storage",
    "network-scoped option",
    "shared across subsites",
    "shared across the network",
    "multisite-aware option",
    "multisite option storage",
]:
    require(include_regex.search(phrase),
            f"option-scope include pattern must recognize drift phrase: {phrase!r}")

literal_rule = next(
    rule for rule in rules["requested_detectors"] if rule["id"] == "wordpress-constant-backed-slug-literal"
)
literal_pattern = literal_rule["literal_pattern"].replace("{value}", re.escape("tool_call"))
literal_regex = re.compile(literal_pattern, re.MULTILINE)
exclude_patterns = [
    pattern.replace("{value}", re.escape("tool_call"))
    for pattern in literal_rule.get("exclude_match_context_patterns", [])
]
exclude_regexes = [re.compile(pattern, re.MULTILINE) for pattern in exclude_patterns]
fixture = (fixture_dir / "src/Runtime/class-demo-event.php").read_text(encoding="utf-8")
matches = [match.group(0) for match in literal_regex.finditer(fixture)]

require(matches, "constant-backed slug detector literal pattern must match slug literals")
require(literal_regex.search("self::TOOL_CALL === 'tool_call'"), "constant-backed slug detector should still flag comparisons")
require(not any(regex.search("self::TOOL_CALL === 'tool_call'") for regex in exclude_regexes), "comparison duplicate must not be excluded")
require(any(regex.search("'tool_call' =>") for regex in exclude_regexes), "array/protocol key literal must be excluded by context")
require(any(regex.search("do_action( 'tool_call'") for regex in exclude_regexes), "event-name literal must be excluded by context")
require("'tool_call' =>" in fixture, "fixture should include array/protocol key literal")
require("do_action( 'tool_call'" in fixture, "fixture should include event-name literal")

i18n_literal_pattern = literal_rule["literal_pattern"].replace("{value}", re.escape("data-machine"))
i18n_literal_regex = re.compile(i18n_literal_pattern, re.MULTILINE)
i18n_exclude_patterns = [
    pattern.replace("{value}", re.escape("data-machine"))
    for pattern in literal_rule.get("exclude_match_context_patterns", [])
]
i18n_exclude_regexes = [re.compile(pattern, re.MULTILINE) for pattern in i18n_exclude_patterns]
i18n_fixture = (fixture_dir / "src/Runtime/class-demo-i18n-text-domain.php").read_text(encoding="utf-8")

require(i18n_literal_regex.search("__( 'Flow', 'data-machine' )"),
        "constant-backed slug detector literal pattern must match i18n text-domain literals before context exclusion")
require(any(regex.search("__( 'Flow', 'data-machine' )") for regex in i18n_exclude_regexes),
        "i18n text-domain literal must be excluded by context")
require(i18n_literal_regex.search("self::TEXT_DOMAIN === 'data-machine'"),
        "constant-backed slug detector should still flag non-i18n duplicate slug literals")
require(not any(regex.search("self::TEXT_DOMAIN === 'data-machine'") for regex in i18n_exclude_regexes),
        "non-i18n duplicate slug literal must not be excluded")
require("__( 'Flow', 'data-machine' )" in i18n_fixture,
        "i18n fixture should include a WordPress translation text-domain literal")
require("self::TEXT_DOMAIN === 'data-machine'" in i18n_fixture,
        "i18n fixture should include a real duplicate slug literal")

# Issue #425 — source_pattern must not match `class` in docblock prose.
# Pre-fix, `(?s).*?` between `class <name>` and `const` would let
# `class has no knowledge` from the docblock match as the class name and
# walk forward to the next const, producing a bogus `has::GROUP` finding.
# The fix anchors `class` to start of line (so docblock continuation
# lines beginning with `*` cannot satisfy it) and requires an opening
# brace before `const` so the match has to live inside a real class body.
source_pattern = literal_rule["source_pattern"]
source_regex = re.compile(source_pattern)
i18n_source_matches = list(source_regex.finditer(i18n_fixture))
require(
    len(i18n_source_matches) == 1,
    f"source_pattern must find the i18n fixture text-domain constant, got {len(i18n_source_matches)}",
)
require(
    i18n_source_matches[0].group("value") == "data-machine",
    f"source_pattern must capture the i18n fixture constant value, got {i18n_source_matches[0].group('value')!r}",
)
recurring_fixture = (fixture_dir / "src/Runtime/class-demo-recurring-scheduler.php").read_text(encoding="utf-8")
require(
    "This class has no knowledge" in recurring_fixture,
    "recurring-scheduler fixture must contain the docblock prose that triggered the parser glitch",
)
source_matches = list(source_regex.finditer(recurring_fixture))
require(
    len(source_matches) == 1,
    f"source_pattern must produce exactly one match on the recurring-scheduler fixture, got {len(source_matches)}",
)
match = source_matches[0]
require(
    match.group("class") == "Demo_Recurring_Scheduler",
    f"source_pattern must capture the real class name, got class={match.group('class')!r}",
)
require(
    match.group("const") == "GROUP",
    f"source_pattern must capture the real const name, got const={match.group('const')!r}",
)
require(
    match.group("value") == "demo-recurring",
    f"source_pattern must capture the real const value, got value={match.group('value')!r}",
)

# Negative case: a class declared only inside a comment body must not match.
comment_only = "\n".join([
    "<?php",
    "/**",
    " * Some module that explains: class FakeStub has a const FOO = 'foo-bar' baked in.",
    " * But there's no real class definition here.",
    " */",
    "$x = 1;",
    "",
])
require(
    not list(source_regex.finditer(comment_only)),
    "source_pattern must not match a class declared only inside a comment block",
)

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
    "src/Auth/class-demo-access-grant.php",
    "src/Auth/class-demo-token.php",
    "src/Auth/class-demo-token-authenticator.php",
    "src/Context/class-demo-context-conflict-resolver.php",
    "src/Context/class-demo-context-injection-policy.php",
    "src/Abilities/CreateAbility.php",
    "src/Abilities/UpdateAbility.php",
    "src/Abilities/FileConstants.php",
    "src/Abilities/BlockSanitizer.php",
    "src/Abilities/PipelineBatchScheduler.php",
    "src/Abilities/FlowThing.php",
    "src/AbilityMismatch/class-demo-create-ability.php",
    "src/AbilityMismatch/class-demo-update-ability.php",
    "src/AbilityMismatch/class-demo-flow-thing.php",
    "src/Options/class-demo-network-drift.php",
    "src/Options/class-demo-single-site-noise.php",
    "src/Options/class-demo-opt-out-marker.php",
    "src/Api/WebhookAuthResolver.php",
    "src/Api/WebhookVerificationResult.php",
    "src/Api/WebhookVerifier.php",
    "src/Api/WebhookRoutes.php",
    "src/Api/WebhookController.php",
    "src/Api/WebhookStatusController.php",
]:
    require((fixture_dir / relative).exists(), f"missing audit detector fixture: {relative}")

print("wordpress audit detector config smoke passed")
PY

AUDIT_JSON="$(mktemp "${TMPDIR:-/tmp}/homeboy-audit-detector-config.XXXXXX.json")"
trap 'rm -f "$AUDIT_JSON"' EXIT
set +e
homeboy audit --force-hot --path "$FIXTURE_DIR" --extension wordpress --only naming_mismatch --output "$AUDIT_JSON" >/dev/null
audit_status=$?
set -e
if [ "$audit_status" -gt 1 ]; then
	exit "$audit_status"
fi

python3 - "$AUDIT_JSON" <<'PY'
import json
import sys

audit_path = sys.argv[1]
with open(audit_path, encoding="utf-8") as handle:
    data = json.load(handle)

findings = data.get("data", {}).get("findings", [])
naming_files = {
    finding.get("file")
    for finding in findings
    if finding.get("kind") == "naming_mismatch"
}

expected = "src/AbilityMismatch/class-demo-flow-thing.php"
if expected not in naming_files:
    raise SystemExit(f"expected true naming_mismatch for {expected}, got {sorted(naming_files)}")

for skipped in [
    "src/Abilities/FileConstants.php",
    "src/Abilities/BlockSanitizer.php",
    "src/Abilities/PipelineBatchScheduler.php",
]:
    if skipped in naming_files:
        raise SystemExit(f"helper/service class should not be a naming_mismatch: {skipped}")

print("wordpress audit naming mismatch fixture smoke passed")
PY
