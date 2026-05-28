#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECTOR="${SCRIPT_DIR}/project-wpgym-eval-row.js"
VALIDATOR="${SCRIPT_DIR}/validate-wpgym-eval-row.js"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wpgym-validator.XXXXXX")"
RESULTS_FILE="${TMP_ROOT}/results.json"
CONFIG_FILE="${TMP_ROOT}/config.json"
TRANSCRIPT_FILE="${TMP_ROOT}/transcript.json"
EPISODE_FILE="${TMP_ROOT}/episode.jsonl"
REPLAY_FILE="${TMP_ROOT}/replay.json"

cleanup() {
	rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

sha256_digest() {
	shasum -a 256 "$1" | cut -d ' ' -f 1
}

write_valid_fixture() {
	printf '%s\n' '{"messages":[]}' > "$TRANSCRIPT_FILE"
	printf '%s\n' '{"row_type":"grader","terminal":true,"result_status":"completed","reward":1,"grade":{"score":1,"max_score":1}}' > "$EPISODE_FILE"
	local transcript_sha episode_sha
	transcript_sha="sha256:$(sha256_digest "$TRANSCRIPT_FILE")"
	episode_sha="sha256:$(sha256_digest "$EPISODE_FILE")"
	jq -n \
		--arg transcriptSha "$transcript_sha" \
		--arg episodeSha "$episode_sha" \
		'{
			sealed_eval_artifact: {
				replay: { format: "jsonl", episode_row_count: 1, tool_audit_event_count: 1 },
				hashes: {
					envelope: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
					artifact_hashes: {
						transcript_json: { sha256: $transcriptSha },
						episode_jsonl: { sha256: $episodeSha }
					}
				}
			}
		}' > "$REPLAY_FILE"

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
						reward: 1,
						grade: { score: 1, max_score: 1 },
						evidence_references: {
							schema: "homeboy/datamachine-agent-evidence-references/v1",
							references: {
								homeboy_result_json: { kind: "json", path: $resultsFile, label: "Homeboy result JSON", source: "homeboy", available: true },
								artifact_verifier_result: { kind: "json", value: { status: "passed" }, label: "Artifact verifier result", source: "runner", available: true },
								workspace_policy_result: { kind: "json", value: { status: "passed" }, label: "Workspace policy result", source: "data-machine-code", available: true },
								grader_result: { kind: "json", value: { score: 1, max_score: 1 }, label: "Grader result", source: "runner", available: true },
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
		success_requires_pr: true,
		wp_gym_eval: { benchmark_mode: true }
	}' > "$CONFIG_FILE"

	node "$PROJECTOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" --benchmark-mode --update-results >/dev/null
}

assert_invalid() {
	local label="$1"
	local expected="$2"
	local mutate_filter="$3"
	write_valid_fixture
	jq "$mutate_filter" "$RESULTS_FILE" > "${RESULTS_FILE}.invalid"
	mv "${RESULTS_FILE}.invalid" "$RESULTS_FILE"
	set +e
	node "$VALIDATOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" >/dev/null 2>"${TMP_ROOT}/${label}.err"
	local status=$?
	set -e
	if [ "$status" -eq 0 ]; then
		echo "ERROR: validator accepted invalid fixture: $label" >&2
		exit 1
	fi
	if ! grep -q "$expected" "${TMP_ROOT}/${label}.err"; then
		echo "ERROR: validator failure for $label did not mention $expected" >&2
		cat "${TMP_ROOT}/${label}.err" >&2
		exit 1
	fi
}

write_valid_fixture
node "$VALIDATOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" >/dev/null

assert_invalid "missing-artifact" "local artifact does not exist" '.scenarios[0].metadata.evidence_references.references.transcript_artifact.path = "missing-transcript.json" | .scenarios[0].metadata.wp_gym_eval_row.orchestration.artifacts.all_references.transcript.path = "missing-transcript.json"'
assert_invalid "remote-artifact" "must be a local artifact" '.scenarios[0].metadata.evidence_references.references.runtime_episode_trace.path = "https://example.com/episode.jsonl" | .scenarios[0].metadata.wp_gym_eval_row.orchestration.artifacts.all_references.episode_trace.path = "https://example.com/episode.jsonl"'
write_valid_fixture
jq 'del(.sealed_eval_artifact.hashes.artifact_hashes.transcript_json)' "$REPLAY_FILE" > "${REPLAY_FILE}.missing-hash"
mv "${REPLAY_FILE}.missing-hash" "$REPLAY_FILE"
set +e
node "$VALIDATOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" >/dev/null 2>"${TMP_ROOT}/missing-hash.err"
missing_hash_status=$?
set -e
if [ "$missing_hash_status" -eq 0 ] || ! grep -q 'missing a matching sha256 hash' "${TMP_ROOT}/missing-hash.err"; then
	echo "ERROR: validator accepted an artifact without a matching hash" >&2
	cat "${TMP_ROOT}/missing-hash.err" >&2
	exit 1
fi

write_valid_fixture
printf '%s\n' '{not json}' > "$EPISODE_FILE"
set +e
node "$VALIDATOR" --results "$RESULTS_FILE" --scenario agent-flow --config "$CONFIG_FILE" >/dev/null 2>"${TMP_ROOT}/invalid-jsonl.err"
invalid_jsonl_status=$?
set -e
if [ "$invalid_jsonl_status" -eq 0 ] || ! grep -q 'Invalid JSONL' "${TMP_ROOT}/invalid-jsonl.err"; then
	echo "ERROR: validator accepted invalid episode JSONL" >&2
	cat "${TMP_ROOT}/invalid-jsonl.err" >&2
	exit 1
fi

assert_invalid "missing-verifier" "verifier" 'del(.scenarios[0].metadata.evidence_references.references.artifact_verifier_result) | del(.scenarios[0].metadata.wp_gym_eval_row.orchestration.artifacts.all_references.verifier)'
assert_invalid "missing-policy" "policy" 'del(.scenarios[0].metadata.evidence_references.references.workspace_policy_result) | del(.scenarios[0].metadata.wp_gym_eval_row.orchestration.artifacts.all_references.policy)'
assert_invalid "grade-mismatch" "Terminal episode grader row" '.scenarios[0].metadata.wp_gym_eval_row.evaluation.grade.score = 0'

echo "✓ wp-gym eval row validator smoke test PASSED"
