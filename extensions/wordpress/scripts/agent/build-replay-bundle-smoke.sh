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
    wp_codebox_blueprint: {
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
generic_has_wp_gym=$(jq -r '.sealed_eval_artifact | has("wp_gym")' "$BUNDLE_PATH")
tool_audit_count=$(jq -r '.sealed_eval_artifact.replay.tool_audit_event_count' "$BUNDLE_PATH")
episode_row_count=$(jq -r '.sealed_eval_artifact.replay.episode_row_count' "$BUNDLE_PATH")
transcript_hash=$(jq -r '.sealed_eval_artifact.hashes.artifact_hashes.transcript_json.sha256 // "missing"' "$BUNDLE_PATH")
episode_hash=$(jq -r '.sealed_eval_artifact.hashes.artifact_hashes.episode_jsonl.sha256 // "missing"' "$BUNDLE_PATH")
envelope_hash=$(jq -r '.sealed_eval_artifact.hashes.envelope // "missing"' "$BUNDLE_PATH")
missing_seams=$(jq -r '.sealed_eval_artifact.integration_seams | join(",")' "$BUNDLE_PATH")
if [ "$sealed_status" != "ready_for_replay" ] || [ "$generic_has_wp_gym" != "false" ] || [ "$tool_audit_count" != "1" ] || [ "$episode_row_count" != "2" ] || [ "$transcript_hash" = "missing" ] || [ "$episode_hash" = "missing" ] || [ "$envelope_hash" = "missing" ]; then
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
if [[ "$missing_seams" != *"runtime_provenance"* ]] || [[ "$missing_seams" != *"workspace_policy_attestation"* ]]; then
	echo "ERROR: expected missing provenance/policy seams to remain explicit" >&2
	cat "$BUNDLE_PATH" >&2
	exit 1
fi

jq -n --arg transcriptFile "$TRANSCRIPT_FILE" '{
	component_id: "example-plugin",
	iterations: 1,
	scenarios: [
		{
			id: "agent-wp-gym",
			label: "Agent wp-gym task",
			source: "config",
			metrics: { reward_mean: 1 },
			artifacts: {
				transcript_json: { path: $transcriptFile, kind: "jsonl" }
			},
			metadata: {
				provider: "openai",
				model: "gpt-example",
				job_status: "completed",
				grade: { score: 1, max_score: 1 },
				reward: 1,
				wp_gym_eval: {
					scenario: {
						id: "navigation-menu-001",
						label: "Create a navigation menu",
						task_family: "navigation"
					},
					status: { outcome: "passed", failure_class: "none" }
				},
				tool_audit_events: [
					{
						schema_version: 1,
						type: "tool_call",
						turn_count: 1,
						tool_name: "client/update_site",
						parameters_sha256: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
						success: true,
						result_status: "success"
					}
				]
			}
		}
	]
}' > "$RESULTS_TMPFILE"

jq -n '{
	component_id: "example-plugin",
	workload_id: "agent-wp-gym",
	provider: "openai",
	model: "gpt-example",
	prompt: "Create a navigation menu.",
	general_rules: ["Follow the task instructions."],
	task_rules: ["Do not modify unrelated content."],
	wp_gym_eval: {
		benchmark_mode: true,
		task_set: {
			id: "wp-admin-tasks",
			version: "2026-05-29",
			benchmark_status: "candidate",
			compatibility_group: "wp-6.8",
			aggregate_score: false,
			headline_score_eligible: false
		},
		grader: {
			success: true,
			checks: [{ name: "menu_exists", status: "passed" }]
		}
	}
}' > "$CONFIG_TMPFILE"

node "$SCRIPT_DIR/build-replay-bundle.js" \
	--results "$RESULTS_TMPFILE" \
	--scenario agent-wp-gym \
	--config "$CONFIG_TMPFILE" \
	--output-dir "$BUNDLE_DIR" >/dev/null

WPGYM_BUNDLE_PATH="$BUNDLE_DIR/agent-wp-gym-replay-bundle.json"
wp_gym_projection=$(jq -r '[
	.sealed_eval_artifact.wp_gym.scenario.id,
	.sealed_eval_artifact.wp_gym.scenario.task_family,
	.sealed_eval_artifact.wp_gym.task_set.id,
	.sealed_eval_artifact.wp_gym.task_set.benchmark_status,
	(.sealed_eval_artifact.wp_gym.grader.success | tostring),
	(.sealed_eval_artifact.wp_gym.grader.reward | tostring),
	.sealed_eval_artifact.wp_gym.status.outcome,
	.sealed_eval_artifact.wp_gym.status.failure_class
] | join(" ")' "$WPGYM_BUNDLE_PATH")
wp_gym_prompt_hash=$(jq -r '.sealed_eval_artifact.wp_gym.scenario.prompt_sha256 // "missing"' "$WPGYM_BUNDLE_PATH")
wp_gym_check_name=$(jq -r '.sealed_eval_artifact.wp_gym.grader.checks[0].name // "missing"' "$WPGYM_BUNDLE_PATH")
if [ "$wp_gym_projection" != "navigation-menu-001 navigation wp-admin-tasks candidate true 1 passed none" ] || [ "$wp_gym_prompt_hash" = "missing" ] || [ "$wp_gym_check_name" != "menu_exists" ]; then
	echo "ERROR: wp-gym sealed eval projection incomplete" >&2
	cat "$WPGYM_BUNDLE_PATH" >&2
	exit 1
fi

jq -n '{
	scenarios: [
		{
			id: "agent-wp-gym-partial",
			name: "Partial wp-gym projection",
			metadata: {
				eval_artifact: {
					run: { job_status: "completed", success_status: "no_changes" },
					grade: {}
				},
				fingerprints: { prompt: { sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } }
			}
		}
	]
}' > "$RESULTS_TMPFILE"

jq -n '{
	component_id: "example-plugin",
	workload_id: "agent-wp-gym-partial",
	provider: "openai",
	model: "gpt-example",
	wp_gym_eval: {
		status: { outcome: "completed" },
		grader: { grade: {}, checks: [] }
	}
}' > "$CONFIG_TMPFILE"

node "$SCRIPT_DIR/build-replay-bundle.js" \
	--results "$RESULTS_TMPFILE" \
	--scenario agent-wp-gym-partial \
	--config "$CONFIG_TMPFILE" \
	--output-dir "$BUNDLE_DIR" >/dev/null

PARTIAL_WPGYM_BUNDLE_PATH="$BUNDLE_DIR/agent-wp-gym-partial-replay-bundle.json"
partial_status_has_outcome=$(jq -r '(.sealed_eval_artifact.wp_gym.status // {}) | has("outcome")' "$PARTIAL_WPGYM_BUNDLE_PATH")
partial_grade_has_score=$(jq -r '(.sealed_eval_artifact.wp_gym.grader.grade // {}) | has("score")' "$PARTIAL_WPGYM_BUNDLE_PATH")
if [ "$partial_status_has_outcome" != "false" ] || [ "$partial_grade_has_score" != "false" ]; then
	echo "ERROR: partial wp-gym status or grade should not be projected" >&2
	cat "$PARTIAL_WPGYM_BUNDLE_PATH" >&2
	exit 1
fi

echo "✓ replay bundle smoke test PASSED"
