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

function homeboy_bench_selected_scenarios(?string $selected = null): array {
    $selected = $selected ?? (getenv('HOMEBOY_BENCH_SCENARIOS') ?: '');
    return array_values(array_filter(array_map('trim', explode(',', $selected)), static fn($scenario) => $scenario !== ''));
}

function homeboy_bench_scenario_selected(string $scenario, ?array $selected = null): bool {
    $selected = $selected ?? homeboy_bench_selected_scenarios();
    return count($selected) === 0 || in_array($scenario, $selected, true);
}

function homeboy_bench_artifact_ref(string $path, ?string $kind = null, ?string $label = null): array {
    if ($path === '') {
        throw new InvalidArgumentException('homeboy_bench_artifact_ref requires a path');
    }

    $ref = ['path' => $path];
    if ($kind !== null && $kind !== '') {
        $ref['kind'] = $kind;
    }
    if ($label !== null && $label !== '') {
        $ref['label'] = $label;
    }
    return $ref;
}

function homeboy_bench_results_envelope(string $component_id, int $iterations, array $scenarios): array {
    return [
        'component_id' => $component_id,
        'iterations' => $iterations,
        'scenarios' => $scenarios,
    ];
}

function homeboy_bench_scenario_inventory_entry(array $scenario): array {
    if (empty($scenario['id'])) {
        throw new InvalidArgumentException('homeboy_bench_scenario_inventory_entry requires an id');
    }

    $entry = [
        'id' => $scenario['id'],
        'iterations' => 0,
        'tags' => $scenario['tags'] ?? [],
        'metrics' => $scenario['metrics'] ?? [],
    ];

    if (array_key_exists('default_iterations', $scenario)) {
        $entry['default_iterations'] = (int) $scenario['default_iterations'];
    }

    foreach (['file', 'source', 'metadata', 'artifacts'] as $key) {
        if (isset($scenario[$key]) && $scenario[$key] !== [] && $scenario[$key] !== '') {
            $entry[$key] = $scenario[$key];
        }
    }

    return $entry;
}

function homeboy_bench_scenario_inventory_envelope(string $component_id, int $default_iterations, array $scenarios): array {
    return homeboy_bench_results_envelope(
        $component_id,
        0,
        array_map(
            static function (array $scenario) use ($default_iterations): array {
                if (!array_key_exists('default_iterations', $scenario)) {
                    $scenario['default_iterations'] = $default_iterations;
                }
                return homeboy_bench_scenario_inventory_entry($scenario);
            },
            $scenarios
        )
    );
}

function homeboy_write_bench_results(string $results_path, string $component_id, int $iterations, array $scenarios): void {
    $json = json_encode(homeboy_bench_results_envelope($component_id, $iterations, $scenarios), JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
    if ($json === false) {
        throw new RuntimeException('json_encode failed: ' . json_last_error_msg());
    }
    $results_dir = dirname($results_path);
    if (!is_dir($results_dir) && !mkdir($results_dir, 0777, true) && !is_dir($results_dir)) {
        throw new RuntimeException("failed to create $results_dir");
    }
    if (file_put_contents($results_path, $json) === false) {
        throw new RuntimeException("failed to write $results_path");
    }
}

function homeboy_write_empty_bench_results(string $results_path, string $component_id, int $iterations = 0): void {
    homeboy_write_bench_results($results_path, $component_id, $iterations, []);
}

function homeboy_write_bench_scenario_inventory(string $results_path, string $component_id, int $default_iterations, array $scenarios): void {
    $json = json_encode(homeboy_bench_scenario_inventory_envelope($component_id, $default_iterations, $scenarios), JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
    if ($json === false) {
        throw new RuntimeException('json_encode failed: ' . json_last_error_msg());
    }
    $results_dir = dirname($results_path);
    if (!is_dir($results_dir) && !mkdir($results_dir, 0777, true) && !is_dir($results_dir)) {
        throw new RuntimeException("failed to create $results_dir");
    }
    if (file_put_contents($results_path, $json) === false) {
        throw new RuntimeException("failed to write $results_path");
    }
}
