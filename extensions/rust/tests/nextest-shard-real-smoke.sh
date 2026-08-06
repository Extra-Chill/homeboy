#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="${HOMEBOY_TESTED_EXTENSION_DIR:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
FIXTURE_DIR="${SCRIPT_DIR}/fixtures/nextest-shard-real"
WORK_DIR="$(mktemp -d -t homeboy-nextest-real.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT
cd "$FIXTURE_DIR"

if ! cargo nextest --version | grep -q '^cargo-nextest 0\.9\.140'; then
    printf 'Expected cargo-nextest exactly 0.9.140; install with cargo install cargo-nextest --version 0.9.140 --locked\n' >&2
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

python3 - "$INVENTORY" "$WORK_DIR/manifest.json" <<'PY'
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
assert results == {"total": 1, "passed": 1, "failed": 0, "skipped": 0, "partial": "rust-shard"}, results
PY

printf 'real nextest shard smoke ok\n'
