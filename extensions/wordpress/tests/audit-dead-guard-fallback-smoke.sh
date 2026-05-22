#!/usr/bin/env bash
#
# Verifies that core's dead_guard detector ignores WordPress-utility guards
# whose else-branch is a natural pure-PHP fallback (the dual-context idiom),
# while still reporting guards around symbols that only ship with WordPress.
#
# The contract under test lives entirely in wordpress.json's
# audit.detector_rules.known_symbols.header_versions list. WordPress utility
# wrappers with natural pure-PHP equivalents must not be declared there;
# WordPress-only types/functions must remain declared.

set -euo pipefail

EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST_PATH="${MANIFEST_PATH:-${EXTENSION_DIR}/wordpress.json}"
FALLBACK_FIXTURE="${EXTENSION_DIR}/tests/fixtures/audit-dead-guard-fallback"
WP_ONLY_FIXTURE="${EXTENSION_DIR}/tests/fixtures/audit-dead-guard-wp-only"

# Step 1: manifest contract — utility wrappers excluded, WP-only kept.
python3 - "$MANIFEST_PATH" <<'PY'
import json
import sys
from pathlib import Path

manifest = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
header_versions = manifest["audit"]["detector_rules"]["known_symbols"]["header_versions"]
declared = {entry["name"] for provider in header_versions for entry in provider.get("symbols", [])}

excluded = [
    "wp_json_encode",
    "wp_generate_uuid4",
    "wp_parse_url",
    "wp_date",
    "wp_timezone",
    "wp_timezone_string",
]
violations = [name for name in excluded if name in declared]
if violations:
    raise SystemExit(
        "WordPress utility wrappers with natural pure-PHP fallbacks must not be "
        "declared in known_symbols.header_versions: "
        + ", ".join(sorted(violations))
    )

required = [
    "register_rest_route",
    "register_block_type",
    "WP_REST_Server",
    "WP_REST_Request",
    "WP_REST_Response",
    "WP_Block_Type_Registry",
    "WP_Block",
    "REST_REQUEST",
]
missing = [name for name in required if name not in declared]
if missing:
    raise SystemExit(
        "WordPress-only symbols must remain in known_symbols.header_versions: "
        + ", ".join(sorted(missing))
    )

print("manifest contract ok")
PY

# Step 2: behavioral contract — only run if a homeboy CLI is available.
# Prefer the Homebrew install over a stale `cargo install` build so behavior is
# reproducible across hosts; respect HOMEBOY_BIN when callers know better.
if [[ -z "${HOMEBOY_BIN:-}" ]]; then
    if [[ -x "/opt/homebrew/bin/homeboy" ]]; then
        HOMEBOY_BIN="/opt/homebrew/bin/homeboy"
    elif [[ -x "/usr/local/bin/homeboy" ]]; then
        HOMEBOY_BIN="/usr/local/bin/homeboy"
    else
        HOMEBOY_BIN="$(command -v homeboy || true)"
    fi
fi
if [[ -z "${HOMEBOY_BIN}" ]]; then
    echo "homeboy CLI unavailable; skipping behavioral audit checks" >&2
    echo "wordpress audit dead-guard fallback smoke passed (manifest only)"
    exit 0
fi

run_audit() {
    local fixture_path="$1"
    local out_path="$2"
    "${HOMEBOY_BIN}" audit --path "${fixture_path}" --extension wordpress --output "${out_path}" --force-hot >/dev/null
}

FALLBACK_REPORT="$(mktemp -t audit-fallback.XXXXXX.json)"
WP_ONLY_REPORT="$(mktemp -t audit-wp-only.XXXXXX.json)"
TMP_FIXTURE_ROOT="$(mktemp -d -t audit-dead-guard-fixtures.XXXXXX)"
trap 'rm -f "${FALLBACK_REPORT}" "${WP_ONLY_REPORT}"; rm -rf "${TMP_FIXTURE_ROOT}"' EXIT

copy_fixture_with_component_config() {
    local source_fixture="$1"
    local target_fixture="$2"
    local component_id="$3"

    mkdir -p "${target_fixture}"
    cp -R "${source_fixture}/." "${target_fixture}/"
    cat > "${target_fixture}/homeboy.json" <<JSON
{
  "id": "${component_id}",
  "extensions": {
    "wordpress": {}
  }
}
JSON
}

FALLBACK_AUDIT_FIXTURE="${TMP_FIXTURE_ROOT}/fallback"
WP_ONLY_AUDIT_FIXTURE="${TMP_FIXTURE_ROOT}/wp-only"
copy_fixture_with_component_config "${FALLBACK_FIXTURE}" "${FALLBACK_AUDIT_FIXTURE}" "audit-dead-guard-fallback"
copy_fixture_with_component_config "${WP_ONLY_FIXTURE}" "${WP_ONLY_AUDIT_FIXTURE}" "audit-dead-guard-wp-only"

run_audit "${FALLBACK_AUDIT_FIXTURE}" "${FALLBACK_REPORT}"
run_audit "${WP_ONLY_AUDIT_FIXTURE}" "${WP_ONLY_REPORT}"

python3 - "$FALLBACK_REPORT" "$WP_ONLY_REPORT" <<'PY'
import json
import sys
from pathlib import Path

fallback = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
wp_only = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))

def files_scanned(report):
    return (report.get("data", {}).get("summary") or {}).get("files_scanned", 0)

# Sanity-guard against an audit binary that fails to recurse src/ — without
# this, a zero-finding wp-only run would look like a known-symbol regression
# instead of a scanning regression.
for label, report in (("fallback", fallback), ("wp_only", wp_only)):
    if files_scanned(report) < 2:
        raise SystemExit(
            f"{label} fixture audit scanned {files_scanned(report)} files; "
            "expected the runner to recurse into src/Runtime/. The homeboy "
            "binary on PATH may be stale or built differently from the "
            "shipping release."
        )

def dead_guards(report):
    return [
        finding
        for finding in report.get("data", {}).get("findings", [])
        if finding.get("kind") == "dead_guard"
    ]

fallback_guards = dead_guards(fallback)
if fallback_guards:
    raise SystemExit(
        "fallback fixture must produce zero dead_guard findings, got: "
        + json.dumps(fallback_guards, indent=2)
    )

wp_only_guards = dead_guards(wp_only)
if not wp_only_guards:
    raise SystemExit(
        "wp-only fixture must still report dead_guard findings; "
        "core's known-symbol pipeline appears broken or the fixture changed shape"
    )

descriptions = " | ".join(g["description"] for g in wp_only_guards)
if "WP_REST_Server" not in descriptions:
    raise SystemExit(
        "wp-only fixture should still flag class_exists('WP_REST_Server'); got: "
        + descriptions
    )
if "register_rest_route" not in descriptions:
    raise SystemExit(
        "wp-only fixture should still flag function_exists('register_rest_route'); got: "
        + descriptions
    )

print(
    "behavioral contract ok: fallback=%d findings, wp_only=%d findings"
    % (len(fallback_guards), len(wp_only_guards))
)
PY

echo "wordpress audit dead-guard fallback smoke passed"
