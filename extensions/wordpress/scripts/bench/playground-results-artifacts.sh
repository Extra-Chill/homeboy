#!/usr/bin/env bash

# Emit wp-rl-friendly artifacts from the WordPress Playground BenchResults
# envelope without changing the core BenchResults contract.

homeboy_wordpress_playground_artifact_dir() {
    local results_file="$1"

    if [ -n "${HOMEBOY_PLAYGROUND_RESULTS_ARTIFACT_DIR:-}" ]; then
        printf '%s\n' "$HOMEBOY_PLAYGROUND_RESULTS_ARTIFACT_DIR"
        return 0
    fi

    dirname "$results_file"
}

homeboy_wordpress_playground_jsonl_filter() {
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

homeboy_wordpress_playground_leaderboard_filter() {
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
        }) | sort_by(-.avg_reward, .avg_duration_ms // 999999999, .provider, .model)) as $groups
        | [
            "# Playground Scenario Leaderboard",
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

homeboy_wordpress_emit_playground_results_artifacts() {
    local bench_results_file="$1"
    local artifact_dir
    local jsonl_file
    local leaderboard_file

    if [ -z "$bench_results_file" ] || [ ! -s "$bench_results_file" ]; then
        return 0
    fi

    artifact_dir="$(homeboy_wordpress_playground_artifact_dir "$bench_results_file")"
    mkdir -p "$artifact_dir"

    jsonl_file="${artifact_dir}/results.jsonl"
    leaderboard_file="${artifact_dir}/leaderboard.md"

    if ! homeboy_wordpress_playground_jsonl_filter < "$bench_results_file" > "$jsonl_file"; then
        echo "ERROR: failed to emit Playground results JSONL artifact at $jsonl_file" >&2
        return 1
    fi

    if ! homeboy_wordpress_playground_leaderboard_filter < "$jsonl_file" > "$leaderboard_file"; then
        echo "ERROR: failed to emit Playground leaderboard artifact at $leaderboard_file" >&2
        return 1
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: [bench:playground] Results JSONL: $jsonl_file"
        echo "DEBUG: [bench:playground] Leaderboard: $leaderboard_file"
    fi
}
