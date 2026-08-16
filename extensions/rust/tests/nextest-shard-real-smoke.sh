#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="${HOMEBOY_TESTED_EXTENSION_DIR:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
FIXTURE_DIR="${SCRIPT_DIR}/fixtures/nextest-shard-real"
WORK_DIR="$(mktemp -d -t homeboy-nextest-real.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT
cd "$FIXTURE_DIR"

# Minimum, not equality. This test drives the real shard-replay path, which
# needs `--message-format libtest-json-plus --message-format-version 0.1` under
# `NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1`. The floor is the oldest release this
# repository has validated that projection against, not a version the code
# requires exactly.
#
# Raise it only when a newer nextest feature is actually depended on. An
# equality check here made every newer toolchain fail on the *version guard*
# rather than on anything this test measures — and in a repository with no PR
# CI, a stale guard and a real shard-replay regression produce the same output.
NEXTEST_MIN_VERSION='0.9.140'

NEXTEST_VERSION_LINE="$(cargo nextest --version 2>/dev/null || true)"
if [ -z "$NEXTEST_VERSION_LINE" ]; then
    printf 'cargo-nextest is not installed. Install at least %s with: cargo install cargo-nextest --version %s --locked\n' \
        "$NEXTEST_MIN_VERSION" "$NEXTEST_MIN_VERSION" >&2
    exit 1
fi

NEXTEST_VERSION="$(printf '%s\n' "$NEXTEST_VERSION_LINE" | sed -n 's/^cargo-nextest \([0-9][0-9.]*\).*$/\1/p')"
if [ -z "$NEXTEST_VERSION" ]; then
    printf 'Could not parse a cargo-nextest version from: %s\n' "$NEXTEST_VERSION_LINE" >&2
    exit 1
fi

# `sort -V` orders version strings; the minimum sorts first unless it is the
# only value, so an equal version passes and an older one is rejected.
if [ "$(printf '%s\n%s\n' "$NEXTEST_MIN_VERSION" "$NEXTEST_VERSION" | sort -V | head -1)" != "$NEXTEST_MIN_VERSION" ]; then
    printf 'cargo-nextest %s is older than the %s minimum this shard-replay test validates against. Install with: cargo install cargo-nextest --version %s --locked\n' \
        "$NEXTEST_VERSION" "$NEXTEST_MIN_VERSION" "$NEXTEST_MIN_VERSION" >&2
    exit 1
fi

cat > "$WORK_DIR/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
should_run_step() { return 0; }
EOF

cat > "$WORK_DIR/command-capture.sh" <<'EOF'
homeboy_run_step_capture() {
    local output_var="$1" exit_var="$2" step_name="$3"
    shift 3
    [ "${1:-}" != -- ] || shift
    local output_file status=0
    output_file="$(mktemp "${TMPDIR:-/tmp}/homeboy-nextest-real.XXXXXX")"
    "$@" >"$output_file" 2>&1 || status=$?
    if [ "$step_name" = 'cargo nextest run' ]; then
        # Keep the nested child stream intact. Stable libtest reports its
        # ignored child only in the suite record, while the consumer run emits
        # this canonical ignored test terminal record.
        printf '%s\n' \
            '{"type":"test","event":"queued","name":"outside::suite$queued"}' \
            '{"type":"test","event":"started","name":"outside::suite$started"}' \
            '{"type":"test","event":"running","name":"outside::suite$running"}' \
            '{"type":"test","event":"ignored","name":"nextest-shard-alpha::nextest_shard_alpha$tests::ignored_child_helper"}' \
            >>"$output_file"
        cp "$output_file" "$HOMEBOY_REAL_NEXTEST_EVENT_STREAM"
    fi
    printf -v "$output_var" '%s' "$output_file"
    printf -v "$exit_var" '%s' "$status"
    return "$status"
}
homeboy_cleanup_step_capture() { rm -f "$1"; }
EOF

cat > "$WORK_DIR/write-test-results.sh" <<'EOF'
homeboy_write_test_results() {
    python3 - "$HOMEBOY_TEST_RESULTS_FILE" "$@" <<'PY'
import json
import sys

path, total, passed, failed, skipped, partial = sys.argv[1:]
json.dump({"total": int(total), "passed": int(passed), "failed": int(failed), "skipped": int(skipped), "partial": partial}, open(path, "w"))
PY
}
EOF

INVENTORY="$WORK_DIR/inventory.json"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_INVENTORY_ONLY=1 \
HOMEBOY_TEST_INVENTORY_FILE="$INVENTORY" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" >/dev/null

if [ -n "${HOMEBOY_PARENT_EXTENSION_DIR:-}" ]; then
    PARENT_INVENTORY="$WORK_DIR/parent-inventory.json"
    PARENT_MANIFEST="$WORK_DIR/parent-manifest.json"
    python3 "$HOMEBOY_PARENT_EXTENSION_DIR/scripts/test-shard-inventory.py" \
        --project "$FIXTURE_DIR" --runner nextest --output "$PARENT_INVENTORY"
    python3 - "$PARENT_INVENTORY" "$PARENT_MANIFEST" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1], encoding="utf-8"))
selected = [test["id"] for test in inventory["tests"] if test["name"] == "tests::selected_parent"]
assert len(selected) == 1, inventory["tests"]
manifest = {key: inventory[key] for key in ("runner", "inventory_fingerprint", "runner_fingerprint", "workspace_fingerprint")}
manifest["schema"] = "homeboy/test-shard-manifest/v1"
manifest["tests"] = selected
json.dump(manifest, open(sys.argv[2], "w"), indent=2)
PY
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_TEST_SHARD_MANIFEST="$PARENT_MANIFEST" \
    HOMEBOY_TEST_RESULTS_FILE="$WORK_DIR/parent-results.json" \
    HOMEBOY_REAL_NEXTEST_EVENT_STREAM="$WORK_DIR/parent-events.jsonl" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
    bash "$EXTENSION_DIR/scripts/test-runner.sh" >"$WORK_DIR/parent-runner.out"
    python3 - "$WORK_DIR/parent-results.json" "$PARENT_MANIFEST" "$WORK_DIR/tampered-parent-manifest.json" "$WORK_DIR/stale-parent-manifest.json" <<'PY'
import json
import sys

results = json.load(open(sys.argv[1], encoding="utf-8"))
assert results == {"total": 1, "passed": 1, "failed": 0, "skipped": 0, "partial": "rust-shard"}, results
manifest = json.load(open(sys.argv[2], encoding="utf-8"))
tampered = dict(manifest, tests=manifest["tests"] + ["nextest-shard-alpha::lib::nextest_shard_alpha::tests::planned_ignored"])
json.dump(tampered, open(sys.argv[3], "w"))
stale = dict(manifest, workspace_fingerprint="stale")
json.dump(stale, open(sys.argv[4], "w"))
PY
    for invalid in "$WORK_DIR/tampered-parent-manifest.json" "$WORK_DIR/stale-parent-manifest.json"; do
        if python3 "$EXTENSION_DIR/scripts/test-shard-inventory.py" --project "$FIXTURE_DIR" --runner nextest --output "$WORK_DIR/invalid.json" --manifest "$invalid" >/dev/null 2>&1; then
            printf 'Expected incompatible parent manifest rejection: %s\n' "$invalid" >&2
            exit 1
        fi
    done
fi

python3 - "$INVENTORY" "$WORK_DIR/manifest.json" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1], encoding="utf-8"))
selected = [test["id"] for test in inventory["tests"] if test["name"] in {"tests::selected_parent", "tests::planned_ignored"}]
assert len(selected) == 2, inventory["tests"]
assert sum(test["expected_outcome"] == "skipped" for test in inventory["tests"] if test["id"] in selected) == 1, inventory["tests"]
manifest = {key: inventory[key] for key in ("runner", "inventory_fingerprint", "runner_fingerprint", "workspace_fingerprint")}
manifest["schema"] = "homeboy/test-shard-manifest/v1"
manifest["tests"] = selected
json.dump(manifest, open(sys.argv[2], "w"), indent=2)
PY

EVENT_STREAM="$WORK_DIR/events.jsonl"
RESULTS="$WORK_DIR/results.json"
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
HOMEBOY_SKIP_LINT=1 \
HOMEBOY_RUST_TEST_RUNNER=nextest \
HOMEBOY_TEST_SHARD_MANIFEST="$WORK_DIR/manifest.json" \
HOMEBOY_TEST_RESULTS_FILE="$RESULTS" \
HOMEBOY_REAL_NEXTEST_EVENT_STREAM="$EVENT_STREAM" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$WORK_DIR/runner-prelude.sh" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$WORK_DIR/command-capture.sh" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WORK_DIR/write-test-results.sh" \
bash "$EXTENSION_DIR/scripts/test-runner.sh" >"$WORK_DIR/runner.out" || {
    python3 - "$WORK_DIR/runner.out" <<'PY'
import sys
print(open(sys.argv[1], encoding="utf-8").read(), file=sys.stderr)
PY
    exit 1
}

python3 - "$EVENT_STREAM" "$RESULTS" <<'PY'
import json
import sys

events = []
for line in open(sys.argv[1], encoding="utf-8"):
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if event.get("type") == "test":
        events.append(event)

assert any(event.get("event") in {"started", "queued", "running"} and event.get("name", "").startswith("outside::") for event in events), events
assert any(event.get("event") == "ignored" and event.get("name", "").endswith("$tests::ignored_child_helper") for event in events), events
assert any(event.get("event") in {"ok", "passed"} and event.get("name", "").endswith("$tests::selected_parent") for event in events), events
results = json.load(open(sys.argv[2], encoding="utf-8"))
assert results == {"total": 2, "passed": 1, "failed": 0, "skipped": 1, "partial": "rust-shard"}, results
PY

printf 'real nextest shard smoke ok\n'
