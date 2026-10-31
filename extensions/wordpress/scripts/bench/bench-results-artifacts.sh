#!/usr/bin/env bash

# Emit wp-rl-friendly artifacts from the WordPress BenchResults envelope without
# changing the core BenchResults contract.

homeboy_wordpress_bench_artifact_dir() {
    local results_file="$1"

    if [ -n "${HOMEBOY_BENCH_RESULTS_ARTIFACT_DIR:-${HOMEBOY_PLAYGROUND_RESULTS_ARTIFACT_DIR:-}}" ]; then
        printf '%s\n' "${HOMEBOY_BENCH_RESULTS_ARTIFACT_DIR:-${HOMEBOY_PLAYGROUND_RESULTS_ARTIFACT_DIR:-}}"
        return 0
    fi

    dirname "$results_file"
}

homeboy_wordpress_bench_jsonl_filter() {
    jq -c \
        --arg env_provider "${HOMEBOY_BENCH_PROVIDER:-${HOMEBOY_PROVIDER:-}}" \
        --arg env_model "${HOMEBOY_BENCH_MODEL:-${HOMEBOY_MODEL:-}}" \
        --arg env_seed "${HOMEBOY_BENCH_SEED:-${HOMEBOY_SEED:-}}" \
        --arg env_run_id "${HOMEBOY_BENCH_RUN_ID:-${HOMEBOY_RUN_ID:-${HOMEBOY_BENCH_INSTANCE_ID:-}}}" \
        '
        . as $root
        | ($root.scenarios // [])[]
        | select((.id // null) != "__bootstrap")
        | . as $scenario
        | ($scenario.metadata // {}) as $metadata
        | ($scenario.metrics // {}) as $metrics
        | {
            component_id: ($root.component_id // null),
            scenario_id: ($scenario.id // null),
            provider: ($metadata.provider // $scenario.provider // ($env_provider | select(. != "")) // null),
            model: ($metadata.model // $scenario.model // ($env_model | select(. != "")) // null),
            seed: ($metadata.seed // $scenario.seed // ($env_seed | select(. != "")) // null),
            run_id: ($metadata.run_id // $scenario.run_id // ($env_run_id | select(. != "")) // null),
            success: (
                $scenario.success
                // $metadata.success
                // (if ($metrics.success_mean? | type) == "number" then $metrics.success_mean >= 1 else null end)
                // (if ($metrics.pass_mean? | type) == "number" then $metrics.pass_mean >= 1 else null end)
            ),
            reward: ($scenario.reward // $metadata.reward // $metrics.reward_mean // null),
            duration_ms: ($scenario.duration_ms // $metadata.duration_ms // $metrics.mean_ms // null),
            turns: ($scenario.turns // $metadata.turns // $metrics.turns_mean // null),
            tokens: ($scenario.tokens // $metadata.tokens // null),
            artifacts: ($scenario.artifacts // null),
            error: ($scenario.error // $scenario.failure // $metadata.error // null)
        }
        '
}

homeboy_wordpress_bench_leaderboard_filter() {
    jq -r -s '
        def num_or_null: if type == "number" then . else null end;
        def fmt_num:
            if . == null then ""
            else ((. * 1000 | round) / 1000 | tostring)
            end;
        def fmt_rate($successes; $total):
            if $total == 0 then ""
            else (((($successes / $total) * 1000) | round) / 10 | tostring) + "%"
            end;
        def key_for($row):
            (($row.provider // "unknown") | tostring) + "\u0000" + (($row.model // "unknown") | tostring);

        . as $rows
        | ($rows | group_by(key_for(.)) | map({
            provider: (.[0].provider // "unknown"),
            model: (.[0].model // "unknown"),
            runs: length,
            successes: (map(select(.success == true)) | length),
            errors: (map(select(.error != null or .success == false)) | length),
            avg_reward: (map(.reward | num_or_null) | map(select(. != null)) | if length == 0 then null else add / length end),
            avg_duration_ms: (map(.duration_ms | num_or_null) | map(select(. != null)) | if length == 0 then null else add / length end)
        }) | sort_by(-(.avg_reward // -1), .avg_duration_ms // 999999999, .provider, .model)) as $groups
        | [
            "# Bench Scenario Leaderboard",
            "",
            "| Provider | Model | Runs | Success | Errors | Avg reward | Avg duration ms |",
            "|---|---|---:|---:|---:|---:|---:|"
        ]
        + ($groups | map(
            "| " + (.provider | tostring) + " | "
            + (.model | tostring) + " | "
            + (.runs | tostring) + " | "
            + fmt_rate(.successes; .runs) + " | "
            + (.errors | tostring) + " | "
            + (.avg_reward | fmt_num) + " | "
            + (.avg_duration_ms | fmt_num) + " |"
        ))
        | .[]
    '
}

homeboy_wordpress_bench_step_series_filter() {
    jq \
        --arg schema "homeboy/wordpress-bench-step-series/v1" \
        '
        def row_array($scenario):
            (
                $scenario.step_series
                // $scenario.metadata.step_series
                // $scenario.metadata.series
                // []
            )
            | if type == "array" then map(select(type == "object")) else [] end;

        def normalize_row($scenario; $index):
            . as $row
            | $row + {
                scenario_id: ($row.scenario_id // $scenario.id // null),
                index: ($row.index // $index)
            }
            | if has("success") then . else
                if (.status // null) == "pass" or (.status // null) == "passed" or (.status // null) == "ok" then . + {success: true}
                elif (.status // null) == "fail" or (.status // null) == "failed" or (.status // null) == "error" then . + {success: false}
                else . end
            end;

        . as $root
        | {
            schema: $schema,
            component_id: ($root.component_id // null),
            generated_from: "homeboy/bench-results/v1",
            series: [
                ($root.scenarios // [])[]
                | select((.id // null) != "__bootstrap")
                | . as $scenario
                | (row_array($scenario)) as $rows
                | ($scenario.artifacts.step_series // null) as $artifact
                | select(($rows | length) > 0 or $artifact != null)
                | {
                    scenario_id: ($scenario.id // null),
                    label: ($scenario.label // null),
                    source: ($scenario.source // null),
                    artifact: $artifact,
                    rows: [
                        $rows
                        | to_entries[]
                        | . as $entry
                        | $entry.value | normalize_row($scenario; $entry.key)
                    ]
                }
            ]
        }
        '
}

homeboy_wordpress_bench_summary_filter() {
	local artifact_dir="$1"
	local jsonl_file="$2"
	local leaderboard_file="$3"
	local series_file="$4"
	local dependency_provenance_file="$5"

	jq \
		--arg schema "homeboy/wordpress-bench-summary/v1" \
		--arg artifactDir "$artifact_dir" \
		--arg jsonlFile "$jsonl_file" \
		--arg leaderboardFile "$leaderboard_file" \
		--arg seriesFile "$series_file" \
		--arg dependencyProvenanceFile "$dependency_provenance_file" \
		--arg runId "${HOMEBOY_BENCH_RUN_ID:-${HOMEBOY_RUN_ID:-${HOMEBOY_BENCH_INSTANCE_ID:-}}}" \
		--arg artifactsCommand "${HOMEBOY_BENCH_ARTIFACTS_COMMAND:-}" \
		--arg rerunCommand "${HOMEBOY_BENCH_RERUN_COMMAND:-}" \
		--arg dependencyRefreshCommand "${HOMEBOY_BENCH_DEPENDENCY_REFRESH_COMMAND:-}" \
		'
		def scenarios: (.scenarios // []) | map(select((.id // null) != "__bootstrap"));
		def artifact_entries($scenario):
			($scenario.artifacts // {})
			| if type == "object" then to_entries else [] end
			| map(. + {scenario_id: ($scenario.id // null)});
		def artifact_path($artifact): ($artifact.path // $artifact.url // null);
		def replay_status_from($value):
			if ($value | type) == "object" then
				$value.status // $value.replayability.status // $value.metadata.replayability.status // null
			else null end;
		def replay_artifact($entry):
			($entry.value // {}) as $artifact
			| select(
				(($entry.key | tostring) | test("replay|blueprint\\.after|after-notes"; "i"))
				or ((artifact_path($artifact) // "") | test("replay|blueprint\\.after|after-notes"; "i"))
			)
			| {
				scenario_id: $entry.scenario_id,
				name: ($entry.key | tostring),
				kind: ($artifact.kind // null),
				label: ($artifact.label // null),
				path: ($artifact.path // null),
				url: ($artifact.url // null),
				viewer: ($artifact.viewer // null),
				status: replay_status_from($artifact)
			};
		. as $root
		| (scenarios) as $scenarios
		| ($scenarios | length) as $total
		| ($scenarios | map(select(
			(.success == true)
			or ((.metrics.success_mean? // null) == 1)
			or ((.metrics.pass_mean? // null) == 1)
		)) | length) as $passed
		| ($scenarios | map(select(
			(.success == false)
			or (.error != null)
			or (.failure != null)
			or ((.metrics.success_mean? // null) == 0)
			or ((.metrics.pass_mean? // null) == 0)
		)) | length) as $failed
		| ([$scenarios[] | artifact_entries(.)[] | replay_artifact(.)]) as $replayArtifacts
		| ($replayArtifacts | map(.status) | map(select(. != null))) as $replayStatuses
		| {
			schema: $schema,
			component_id: ($root.component_id // null),
			run_id: (if $runId == "" then null else $runId end),
			verdict: (if $failed > 0 then "fail" elif $total > 0 and $passed == $total then "pass" else "partial" end),
			score: {
				passed: $passed,
				failed: $failed,
				total: $total,
				rate: (if $total == 0 then null else $passed / $total end)
			},
			replayability: {
				status: (
					$root.replayability.status
					// (if ($replayStatuses | length) > 0 then
						if any($replayStatuses[]; test("partial|incomplete|missing|fail"; "i")) then "partial"
						elif any($replayStatuses[]; test("ready|pass|complete"; "i")) then "ready"
						else "reported" end
					elif ($replayArtifacts | length) > 0 then "artifact_available"
					else "not_reported" end)
				),
				artifacts: $replayArtifacts
			},
			dependencies: {
				prepared: ($root.metadata.prepared_dependencies // $root.prepared_dependencies // []),
				build_failures: ($root.metadata.dependency_build_failures // $root.dependency_build_failures // []),
				provenance_artifact: (if $dependencyProvenanceFile == "" then null else $dependencyProvenanceFile end)
			},
			artifacts: {
				directory: $artifactDir,
				results_jsonl: $jsonlFile,
				leaderboard_markdown: $leaderboardFile,
				step_series_json: $seriesFile
			},
			next_steps: {
				artifact_fetch_command: (if $artifactsCommand != "" then $artifactsCommand elif $runId != "" then "homeboy runs artifacts " + $runId else null end),
				rerun_command: (if $rerunCommand == "" then null else $rerunCommand end),
				dependency_refresh_command: (if $dependencyRefreshCommand == "" then null else $dependencyRefreshCommand end)
			}
		}
		'
}

homeboy_wordpress_emit_bench_results_artifacts() {
	local bench_results_file="$1"
	local artifact_dir
	local jsonl_file
	local leaderboard_file
	local series_file
	local summary_file
	local baseline_results_file
	local codebox_memory_report_file
	local codebox_thresholds_json
	local webperf_summary_json_file
	local webperf_summary_markdown_file
	local webperf_summary_metrics_json
	local webperf_summary_scenarios_json
	local dependency_provenance_file
	local dependency_provenance_reference

    if [ -z "$bench_results_file" ] || [ ! -s "$bench_results_file" ]; then
        return 0
    fi

    artifact_dir="$(homeboy_wordpress_bench_artifact_dir "$bench_results_file")"
    mkdir -p "$artifact_dir"

	jsonl_file="${artifact_dir}/results.jsonl"
	leaderboard_file="${artifact_dir}/leaderboard.md"
	series_file="${artifact_dir}/series.json"
	summary_file="${artifact_dir}/bench-summary.json"
	dependency_provenance_file="${artifact_dir}/bench-dependency-provenance.json"
	dependency_provenance_reference=""
	[ -s "$dependency_provenance_file" ] && dependency_provenance_reference="$dependency_provenance_file"
	codebox_memory_report_file="${artifact_dir}/codebox-browser-memory-comparison.md"
	webperf_summary_json_file="${artifact_dir}/webperf-evidence-summary.json"
	webperf_summary_markdown_file="${artifact_dir}/webperf-evidence-summary.md"

    if ! homeboy_wordpress_bench_jsonl_filter < "$bench_results_file" > "$jsonl_file"; then
        echo "ERROR: failed to emit bench results JSONL artifact at $jsonl_file" >&2
        return 1
    fi

	if ! homeboy_wordpress_bench_leaderboard_filter < "$jsonl_file" > "$leaderboard_file"; then
		echo "ERROR: failed to emit bench leaderboard artifact at $leaderboard_file" >&2
		return 1
	fi

	if ! homeboy_wordpress_bench_step_series_filter < "$bench_results_file" > "$series_file"; then
		echo "ERROR: failed to emit bench step-series artifact at $series_file" >&2
		return 1
	fi

	if ! homeboy_wordpress_bench_summary_filter "$artifact_dir" "$jsonl_file" "$leaderboard_file" "$series_file" "$dependency_provenance_reference" < "$bench_results_file" > "$summary_file"; then
		echo "ERROR: failed to emit bench summary artifact at $summary_file" >&2
		return 1
	fi

	baseline_results_file="${HOMEBOY_BENCH_BASELINE_RESULTS_FILE:-${HOMEBOY_CODEBOX_MEMORY_BASELINE_RESULTS_FILE:-}}"
	if [ -n "$baseline_results_file" ] && [ -s "$baseline_results_file" ]; then
		codebox_thresholds_json="${HOMEBOY_CODEBOX_MEMORY_THRESHOLDS_JSON:-${HOMEBOY_BENCH_CODEBOX_MEMORY_THRESHOLDS_JSON:-{}}}"
		if ! node "${SCRIPT_DIR}/../../lib/codebox-memory-report.js" \
			--baseline "$baseline_results_file" \
			--candidate "$bench_results_file" \
			--thresholds-json "$codebox_thresholds_json" \
			> "$codebox_memory_report_file"; then
			echo "ERROR: failed to emit Codebox browser memory comparison at $codebox_memory_report_file" >&2
			return 1
		fi

		webperf_summary_metrics_json="${HOMEBOY_WEBPERF_SUMMARY_METRICS_JSON:-${HOMEBOY_BENCH_WEBPERF_SUMMARY_METRICS_JSON:-[]}}"
		webperf_summary_scenarios_json="${HOMEBOY_WEBPERF_SUMMARY_SCENARIOS_JSON:-${HOMEBOY_BENCH_WEBPERF_SUMMARY_SCENARIOS_JSON:-[]}}"
		if ! node "${SCRIPT_DIR}/../../lib/webperf-evidence-summary.js" \
			--baseline "$baseline_results_file" \
			--candidate "$bench_results_file" \
			--metrics "$webperf_summary_metrics_json" \
			--scenarios "$webperf_summary_scenarios_json" \
			--output-json "$webperf_summary_json_file" \
			--output-markdown "$webperf_summary_markdown_file"; then
			echo "ERROR: failed to emit webperf evidence summary at $webperf_summary_json_file" >&2
			return 1
		fi
	fi

	if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
		echo "DEBUG: [bench] Results JSONL: $jsonl_file"
		echo "DEBUG: [bench] Leaderboard: $leaderboard_file"
		echo "DEBUG: [bench] Step series: $series_file"
		echo "DEBUG: [bench] Summary: $summary_file"
		[ -s "$codebox_memory_report_file" ] && echo "DEBUG: [bench] Codebox browser memory comparison: $codebox_memory_report_file"
		[ -s "$webperf_summary_json_file" ] && echo "DEBUG: [bench] Webperf evidence summary: $webperf_summary_json_file"
	fi
}
