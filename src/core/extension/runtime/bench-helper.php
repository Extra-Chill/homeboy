<?php
/** Shared BenchResults helpers for PHP extension runners. */

/** R-7 percentile, the runner contract used by Homeboy BenchResults producers. */
function homeboy_bench_percentile(array $sorted_values, float $p): float {
    $n = count($sorted_values);
    if ($n === 0) {
        return 0.0;
    }
    if ($n === 1) {
        return (float) $sorted_values[0];
    }

    $rank = $p * ($n - 1);
    $lo = (int) floor($rank);
    $hi = (int) ceil($rank);
    if ($lo === $hi) {
        return (float) $sorted_values[$lo];
    }

    $frac = $rank - $lo;
    return (float) ($sorted_values[$lo] * (1 - $frac) + $sorted_values[$hi] * $frac);
}

/** scenario slug helper: turn a workload basename into a stable BenchScenario id. */
function homeboy_bench_scenario_id(string $basename): string {
    $name = preg_replace('/\.[^.]+$/', '', $basename);
    $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
    $name = strtolower($name);
    $name = preg_replace('/[^a-z0-9]+/', '-', $name);
    return trim($name, '-');
}

function homeboy_bench_results_envelope(string $component_id, int $iterations, array $scenarios): array {
    return [
        'component_id' => $component_id,
        'iterations' => $iterations,
        'scenarios' => $scenarios,
    ];
}

function homeboy_write_bench_results(string $results_path, string $component_id, int $iterations, array $scenarios): void {
    $json = json_encode(homeboy_bench_results_envelope($component_id, $iterations, $scenarios), JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
    if ($json === false) {
        throw new RuntimeException('json_encode failed: ' . json_last_error_msg());
    }
    if (file_put_contents($results_path, $json) === false) {
        throw new RuntimeException("failed to write $results_path");
    }
}

function homeboy_write_empty_bench_results(string $results_path, string $component_id, int $iterations = 0): void {
    homeboy_write_bench_results($results_path, $component_id, $iterations, []);
}
