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

homeboy_wordpress_emit_bench_results_artifacts() {
	local bench_results_file="$1"
	local artifact_dir
	local jsonl_file
	local leaderboard_file
	local series_file
	local baseline_results_file
	local codebox_memory_report_file
	local codebox_thresholds_json
	local webperf_summary_json_file
	local webperf_summary_markdown_file
	local webperf_summary_metrics_json
	local webperf_summary_scenarios_json

    if [ -z "$bench_results_file" ] || [ ! -s "$bench_results_file" ]; then
        return 0
    fi

    artifact_dir="$(homeboy_wordpress_bench_artifact_dir "$bench_results_file")"
    mkdir -p "$artifact_dir"

	jsonl_file="${artifact_dir}/results.jsonl"
	leaderboard_file="${artifact_dir}/leaderboard.md"
	series_file="${artifact_dir}/series.json"
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
		[ -s "$codebox_memory_report_file" ] && echo "DEBUG: [bench] Codebox browser memory comparison: $codebox_memory_report_file"
		[ -s "$webperf_summary_json_file" ] && echo "DEBUG: [bench] Webperf evidence summary: $webperf_summary_json_file"
	fi
}
