#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECTOR="${SCRIPT_DIR}/project-wpgym-eval-row.js"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wpgym-row.XXXXXX")"
RESULTS_FILE="${TMP_ROOT}/results.json"
CONFIG_FILE="${TMP_ROOT}/config.json"
TRANSCRIPT_FILE="${TMP_ROOT}/transcript.json"
EPISODE_FILE="${TMP_ROOT}/episode.jsonl"
REPLAY_FILE="${TMP_ROOT}/replay.json"

cleanup() {
	rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

printf '%s\n' '{"messages":[]}' > "$TRANSCRIPT_FILE"
printf '%s\n' '{"row_type":"grader","result_status":"completed"}' > "$EPISODE_FILE"
printf '%s\n' '{"sealed_eval_artifact":{"replay":{"format":"jsonl","episode_row_count":1,"tool_audit_event_count":1}}}' > "$REPLAY_FILE"

jq -n \
	--arg transcriptFile "$TRANSCRIPT_FILE" \
	--arg episodeFile "$EPISODE_FILE" \
	--arg replayFile "$REPLAY_FILE" \
	--arg resultsFile "$RESULTS_FILE" \
	'{
		component_id: "datamachine-agent-ci-driver",
		scenarios: [
			{
				id: "agent-flow",
				label: "Agent flow",
				metrics: { reward_mean: 1 },
				artifacts: {
					episode_jsonl: { path: $episodeFile, kind: "jsonl", label: "Episode JSONL" },
					replay_bundle: { path: $replayFile, kind: "json", label: "Replay bundle" }
				},
				metadata: {
					job_id: 123,
					job_status: "completed",
					success_status: "pr_opened",
					target_repo: "Extra-Chill/example",
					agent_slug: "example-agent",
					provider: "openai",
					model: "gpt-example",
					grade: { score: 1, max_score: 1 },
					transcript_artifacts: { json: $transcriptFile },
					evidence_references: {
						schema: "homeboy/datamachine-agent-evidence-references/v1",
						references: {
							homeboy_result_json: { kind: "json", path: $resultsFile, label: "Homeboy result JSON", source: "homeboy", available: true },
							artifact_verifier_result: { kind: "json", value: { status: "passed" }, label: "Artifact verifier result", source: "runner", available: true },
							workspace_policy_result: { kind: "json", value: { status: "passed" }, label: "Workspace policy result", source: "data-machine-code", available: true },
							runtime_episode_trace: { kind: "jsonl", path: $episodeFile, label: "Runtime episode trace", source: "homeboy", available: true },
							transcript_artifact: { kind: "json", path: $transcriptFile, label: "Transcript artifact", source: "data-machine", available: true },
							replay_bundle_artifact: { kind: "json", path: $replayFile, label: "Replay bundle artifact", source: "homeboy", available: true },
							pull_request: { kind: "url", value: "https://github.com/Extra-Chill/example/pull/1", label: "Pull request URL", source: "github", available: true },
							workflow_run: { kind: "url", path: "https://github.com/Extra-Chill/example/actions/runs/1", label: "Workflow run", source: "github", available: true }
						},
						compatibility_gaps: []
					}
				}
			}
		]
	}' > "$RESULTS_FILE"

jq -n '{
	workload_id: "agent-flow",
	workload_label: "Agent flow",
	agent_slug: "example-agent",
	target_repo: "Extra-Chill/example",
	provider: "openai",
	model: "gpt-example",
	prompt: "Do the thing.",
	success_requires_pr: true
}' > "$CONFIG_FILE"

node "$PROJECTOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" --update-results >/dev/null

status="$(jq -r '.scenarios[] | select(.id == "agent-flow") | .metadata.wp_gym_eval_row.status' "$RESULTS_FILE")"
schema="$(jq -r '.scenarios[] | select(.id == "agent-flow") | .metadata.wp_gym_eval_row.schema_name' "$RESULTS_FILE")"
orchestration_pr="$(jq -r '.scenarios[] | select(.id == "agent-flow") | .metadata.wp_gym_eval_row.orchestration.github.pull_request' "$RESULTS_FILE")"
eval_has_orchestration="$(jq -r '.scenarios[] | select(.id == "agent-flow") | .metadata.wp_gym_eval_row.evaluation | has("github") or has("workflow") or has("pull_request")' "$RESULTS_FILE")"
if [ "$status" != "ready" ] || [ "$schema" != "wp-gym.eval_artifact_row" ] || [ "$orchestration_pr" != "https://github.com/Extra-Chill/example/pull/1" ] || [ "$eval_has_orchestration" != "false" ]; then
	echo "ERROR: wp-gym eval projection did not preserve expected row shape" >&2
	cat "$RESULTS_FILE" >&2
	exit 1
fi

jq 'del(.scenarios[0].metadata.evidence_references.references.transcript_artifact)' "$RESULTS_FILE" > "${RESULTS_FILE}.missing"
mv "${RESULTS_FILE}.missing" "$RESULTS_FILE"

set +e
HOMEBOY_WPGYM_BENCHMARK_MODE=1 node "$PROJECTOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" --update-results >/dev/null 2>"${TMP_ROOT}/benchmark.err"
benchmark_status=$?
set -e
if [ "$benchmark_status" -eq 0 ]; then
	echo "ERROR: benchmark mode accepted a missing transcript reference" >&2
	cat "${TMP_ROOT}/benchmark.err" >&2
	exit 1
fi
if ! grep -q 'transcript' "${TMP_ROOT}/benchmark.err"; then
	echo "ERROR: benchmark-mode failure did not report the missing transcript gap" >&2
	cat "${TMP_ROOT}/benchmark.err" >&2
	exit 1
fi

echo "✓ wp-gym eval row projection smoke test PASSED"
