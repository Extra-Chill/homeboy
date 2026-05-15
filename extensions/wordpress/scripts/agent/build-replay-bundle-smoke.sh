#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-replay-results.XXXXXX.json")
CONFIG_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-replay-config.XXXXXX.json")
BUNDLE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/homeboy-replay-bundles.XXXXXX")
TRANSCRIPT_FILE="$BUNDLE_DIR/session-transcript.jsonl"
cleanup() {
	rm -f "$RESULTS_TMPFILE" "$CONFIG_TMPFILE"
	rm -rf "$BUNDLE_DIR"
}
trap cleanup EXIT

printf '%s\n' '{"role":"user","content":"Run the task."}' > "$TRANSCRIPT_FILE"

jq -n --arg transcriptFile "$TRANSCRIPT_FILE" '{
	component_id: "example-plugin",
	iterations: 1,
	scenarios: [
		{
			id: "agent-failure",
			source: "config",
			metrics: { config_present_mean: 1 },
			artifacts: {
				transcript_json: { path: $transcriptFile, kind: "jsonl" }
			},
			metadata: {
				provider: "openai",
				model: "gpt-example",
				job_status: "failed",
				error: "grader failed",
				tool_audit_events: [
					{
						schema_version: 1,
						type: "tool_call",
						turn_count: 1,
						tool_name: "client/search_docs",
						tool_source: "client",
						parameters_sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
						parameters_redacted: true,
						success: true,
						result_status: "success",
						result_sha256: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
					}
				],
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
EPISODE_PATH="$BUNDLE_DIR/agent-failure-episode.jsonl"
if [ ! -s "$BUNDLE_PATH" ]; then
    echo "ERROR: replay bundle was not written" >&2
    exit 1
fi
if [ ! -s "$EPISODE_PATH" ]; then
	echo "ERROR: episode JSONL was not written" >&2
	exit 1
fi

if grep -q 'should-not-leak' "$BUNDLE_PATH"; then
    echo "ERROR: replay bundle leaked a secret value" >&2
    cat "$BUNDLE_PATH" >&2
    exit 1
fi

artifact_path=$(jq -r '.scenarios[] | select(.id == "agent-failure") | .artifacts.replay_bundle.path // "missing"' "$RESULTS_TMPFILE")
episode_path=$(jq -r '.scenarios[] | select(.id == "agent-failure") | .artifacts.episode_jsonl.path // "missing"' "$RESULTS_TMPFILE")
if [ "$artifact_path" = "missing" ] || [ "$episode_path" = "missing" ]; then
    echo "ERROR: replay artifacts were not attached to results" >&2
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

sealed_status=$(jq -r '.sealed_eval_artifact.status' "$BUNDLE_PATH")
tool_audit_count=$(jq -r '.sealed_eval_artifact.replay.tool_audit_event_count' "$BUNDLE_PATH")
episode_row_count=$(jq -r '.sealed_eval_artifact.replay.episode_row_count' "$BUNDLE_PATH")
transcript_hash=$(jq -r '.sealed_eval_artifact.hashes.artifact_hashes.transcript_json.sha256 // "missing"' "$BUNDLE_PATH")
episode_hash=$(jq -r '.sealed_eval_artifact.hashes.artifact_hashes.episode_jsonl.sha256 // "missing"' "$BUNDLE_PATH")
envelope_hash=$(jq -r '.sealed_eval_artifact.hashes.envelope // "missing"' "$BUNDLE_PATH")
missing_seams=$(jq -r '.sealed_eval_artifact.integration_seams | join(",")' "$BUNDLE_PATH")
if [ "$sealed_status" != "ready_for_replay" ] || [ "$tool_audit_count" != "1" ] || [ "$episode_row_count" != "2" ] || [ "$transcript_hash" = "missing" ] || [ "$episode_hash" = "missing" ] || [ "$envelope_hash" = "missing" ]; then
	echo "ERROR: sealed eval artifact envelope incomplete" >&2
	cat "$BUNDLE_PATH" >&2
	exit 1
fi
action_row=$(jq -r 'select(.row_type == "action") | .action_name + " " + .args_sha256 + " " + .result_status' "$EPISODE_PATH")
grader_row=$(jq -r 'select(.row_type == "grader") | .actor + " " + (.terminal|tostring) + " " + .result_status' "$EPISODE_PATH")
shared_policy_hash=$(jq -r 'select(.row_type == "action") | .shared.tool_policy_sha256 // "missing"' "$EPISODE_PATH")
if [ "$action_row" != "client/search_docs sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa success" ] || [ "$grader_row" != "grader true failed" ] || [ "$shared_policy_hash" = "missing" ]; then
	echo "ERROR: episode JSONL rows incomplete" >&2
	cat "$EPISODE_PATH" >&2
	exit 1
fi
if [[ "$missing_seams" != *"datamachine_provenance"* ]] || [[ "$missing_seams" != *"datamachine_code_policy_attestation"* ]]; then
	echo "ERROR: expected missing provenance/policy seams to remain explicit" >&2
	cat "$BUNDLE_PATH" >&2
	exit 1
fi

echo "✓ replay bundle smoke test PASSED"
