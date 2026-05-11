#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-replay-results.XXXXXX.json")
CONFIG_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-replay-config.XXXXXX.json")
BUNDLE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-replay-bundles.XXXXXX")
cleanup() {
    rm -f "$RESULTS_TMPFILE" "$CONFIG_TMPFILE"
    rm -rf "$BUNDLE_DIR"
}
trap cleanup EXIT

jq -n '{
    component_id: "example-plugin",
    iterations: 1,
    scenarios: [
        {
            id: "agent-failure",
            source: "config",
            metrics: { config_present_mean: 1 },
            artifacts: {
                transcript_json: { path: "transcripts/session.json", kind: "json" }
            },
            metadata: {
                provider: "openai",
                model: "gpt-example",
                job_status: "failed",
                error: "grader failed",
                github_token: "should-not-leak"
            }
        }
    ]
}' > "$RESULTS_TMPFILE"

jq -n '{
    component_id: "example-plugin",
    workload_id: "agent-failure",
    provider: "openai",
    model: "gpt-example",
    seed: 123,
    prompt: "Reproduce the failure.",
    openai_api_key: "should-not-leak",
    playground_blueprint: {
        steps: [
            { step: "login", username: "admin", password: "should-not-leak" }
        ]
    }
}' > "$CONFIG_TMPFILE"

node "$SCRIPT_DIR/build-replay-bundle.js" \
    --results "$RESULTS_TMPFILE" \
    --scenario agent-failure \
    --config "$CONFIG_TMPFILE" \
    --output-dir "$BUNDLE_DIR" \
    --update-results >/dev/null

BUNDLE_PATH="$BUNDLE_DIR/agent-failure-replay-bundle.json"
if [ ! -s "$BUNDLE_PATH" ]; then
    echo "ERROR: replay bundle was not written" >&2
    exit 1
fi

if grep -q 'should-not-leak' "$BUNDLE_PATH"; then
    echo "ERROR: replay bundle leaked a secret value" >&2
    cat "$BUNDLE_PATH" >&2
    exit 1
fi

artifact_path=$(jq -r '.scenarios[] | select(.id == "agent-failure") | .artifacts.replay_bundle.path // "missing"' "$RESULTS_TMPFILE")
if [ "$artifact_path" = "missing" ]; then
    echo "ERROR: replay bundle artifact was not attached to results" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

review_available=$(jq -r '.scenarios[] | select(.id == "agent-failure") | .metadata.playground_review.available' "$RESULTS_TMPFILE")
if [ "$review_available" != "false" ]; then
    echo "ERROR: expected unavailable Playground review fallback" >&2
    cat "$RESULTS_TMPFILE" >&2
    exit 1
fi

final_state_available=$(jq -r '.final_state.available' "$BUNDLE_PATH")
if [ "$final_state_available" != "false" ]; then
    echo "ERROR: expected final_state.available=false" >&2
    cat "$BUNDLE_PATH" >&2
    exit 1
fi

echo "✓ replay bundle smoke test PASSED"
