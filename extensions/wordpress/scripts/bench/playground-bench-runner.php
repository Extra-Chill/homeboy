<?php
/**
 * Playground bench runner template.
 *
 * Rendered by scripts/bench/bench-runner-playground.sh via sed substitution
 * of {{PLUGIN_SLUG}}, {{COMPONENT_ID}}, {{ITERATIONS}}, and
 * {{PLAYGROUND_DEP_MOUNTS}} before being mounted into the Playground VFS as
 * /runner.php.
 *
 * BOOT PATH
 *
 * Reuses the four shared bootstrap stages from
 * /homeboy-extension/scripts/lib/playground-bootstrap.php so bench measures
 * the *same* WordPress that tests run against. A regression in `boot` or
 * `install` is therefore visible to both runners simultaneously — the only
 * way bench-vs-test comparisons can be honest.
 *
 * WORKLOAD CONTRACT
 *
 * Each workload file under tests/bench/*.php must `return` a `callable`
 * (typically a closure or `Closure::fromCallable`):
 *
 *     <?php
 *     return function (): array {
 *         // ... do measurable work ...
 *         return ['posts_processed' => 1000];  // optional metadata
 *     };
 *
 * Why `return` instead of `function bench_main()`? Two workloads in the
 * same Playground process can't both define a global `bench_main` —
 * `require_once` doesn't help here because the second file would still
 * try to redeclare. Returning a callable scopes each workload's body
 * lexically, so two workloads coexist cleanly in one process.
 *
 * The runner times wall-clock around the callable, captures peak memory
 * after, and aggregates per-iteration measurements into p50/p95/p99/mean/
 * min/max for the BenchResults envelope. Workloads may also return
 * `['metrics' => ['rows' => 10], 'metadata' => ['phase' => 'warm']]`;
 * numeric custom metrics are aggregated with the same percentile machinery
 * and the latest metadata payload is attached to the scenario.
 *
 * OUTPUT CONTRACT
 *
 * Writes the BenchResults JSON envelope to .pg-bench-results.json under
 * the plugin path (host-visible via the bash runner's mount). The shape
 * matches homeboy core's `extension/bench/parsing.rs::BenchResults`:
 *
 *   { "component_id", "iterations", "scenarios": [
 *       { "id", "file", "iterations", "metrics": {p50_ms, p95_ms, ...},
 *         "memory": { "peak_bytes" } }
 *   ] }
 *
 * Stage diagnostics (boot, install, load_deps, load_component,
 * discover_workloads, run_workloads, emit_results) go to
 * .pg-bench-result.txt via pg_log/pg_stage_* — same shape as the test
 * runner so the bash side can classify failures with the same logic.
 */

error_reporting(E_ALL);
ini_set('display_errors', '1');
ini_set('display_startup_errors', '1');

$plugin_path = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}';
$result_file = "$plugin_path/.pg-bench-result{{RESULT_SUFFIX}}.txt";
$current_stage = 'preboot';

// Multi-instance + shared-state context (homeboy#1508).
//
// Workloads can opt in to concurrent-writer or crash-recovery patterns
// by reading these constants. They're always defined; on single-instance
// runs without --shared-state HOMEBOY_BENCH_SHARED_STATE === '' so a
// workload can do `if (HOMEBOY_BENCH_SHARED_STATE !== '') { ... }` to
// branch.
if (!defined('HOMEBOY_BENCH_SHARED_STATE')) {
    define('HOMEBOY_BENCH_SHARED_STATE', '{{SHARED_STATE_PATH}}');
}
if (!defined('HOMEBOY_BENCH_INSTANCE_ID')) {
    define('HOMEBOY_BENCH_INSTANCE_ID', (int) '{{INSTANCE_ID}}');
}
if (!defined('HOMEBOY_BENCH_CONCURRENCY')) {
    define('HOMEBOY_BENCH_CONCURRENCY', (int) '{{CONCURRENCY}}');
}
if (!defined('HOMEBOY_BENCH_LIST_ONLY')) {
    define('HOMEBOY_BENCH_LIST_ONLY', '{{LIST_ONLY}}' === 'true');
}

require_once '/homeboy-extension/scripts/lib/playground-bootstrap.php';

pg_install_diagnostics_handlers();

// Stages 1-4: shared boot path, identical to test runner.
//
// Component-declared wp-config defines are forwarded as a JSON-encoded
// associative array via the {{WP_CONFIG_DEFINES_JSON}} placeholder. The
// dispatcher reads the `wp_config_defines` setting from the merged
// settings JSON and passes it through; an empty object ("{}") is the
// no-op case. Decode here and hand to pg_run_boot_stage().
$wp_config_defines_raw = '{{WP_CONFIG_DEFINES_JSON}}';
$wp_config_defines = json_decode($wp_config_defines_raw, true);
if (!is_array($wp_config_defines)) {
    $wp_config_defines = [];
}
$config_path = pg_run_boot_stage(['extra_defines' => $wp_config_defines]);

// Component-declared bench env vars. Host shell env doesn't propagate
// across the wp-playground-cli sandbox boundary, so workloads' getenv()
// calls return false for anything the parent shell set. The dispatcher
// extracts the `bench_env` setting from HOMEBOY_SETTINGS_JSON and the
// runner calls putenv() for each entry here, before workload discovery,
// so getenv() resolves correctly inside workloads.
//
// Empty object is the no-op case — components that don't declare
// bench_env see no behavioural change.
$bench_env_raw = '{{BENCH_ENV_JSON}}';
$bench_env = json_decode($bench_env_raw, true);
if (is_array($bench_env)) {
    foreach ($bench_env as $name => $value) {
        if (is_string($name) && preg_match('/^[A-Za-z_][A-Za-z0-9_]*$/', $name)) {
            // putenv() takes "NAME=value" — coerce the value to string,
            // var_export-style for non-scalars (workloads expect string
            // input to getenv() per PHP convention).
            $string_value = is_scalar($value) ? (string) $value : json_encode($value);
            putenv($name . '=' . $string_value);
            // Also populate $_ENV so workloads using $_ENV['NAME'] work.
            $_ENV[$name] = $string_value;
        } else {
            pg_log("NOTICE: skipping invalid bench_env key: " . var_export($name, true));
        }
    }
}

pg_run_install_stage(['config_path' => $config_path]);
pg_run_load_deps_stage(['dep_mounts' => '{{PLAYGROUND_DEP_MOUNTS}}']);
pg_run_load_component_stage(['plugin_path' => $plugin_path]);

/** Slugify a workload basename into a scenario id ("BulkImport.php" → "bulk-import"). */
function pg_bench_scenario_id(string $basename): string {
    $name = preg_replace('/\.php$/i', '', $basename);
    $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
    $name = strtolower($name);
    $name = preg_replace('/[^a-z0-9]+/', '-', $name);
    return trim($name, '-');
}

/**
 * Normalize the optional bench_workloads setting into scenario IDs.
 *
 * Accepts either a JSON array (`["boot-timing"]`) or a comma-separated
 * string (`"boot-timing,read-heavy"`). Values are slugified the same way
 * workload basenames are, so callers can pass `Boot Timing` or
 * `boot-timing` and hit the same workload ID.
 */
function pg_bench_normalize_workload_filter(string $raw): array {
    if ($raw === '' || $raw === 'null') {
        return [];
    }

    $decoded = json_decode($raw, true);
    if (json_last_error() !== JSON_ERROR_NONE) {
        throw new RuntimeException('bench_workloads must be a JSON array or comma-separated string');
    }
    if ($decoded === null || $decoded === '') {
        return [];
    }

    $values = is_array($decoded) ? $decoded : explode(',', (string) $decoded);
    $normalized = [];
    foreach ($values as $value) {
        if (!is_scalar($value)) {
            throw new RuntimeException('bench_workloads entries must be strings');
        }
        $id = pg_bench_scenario_id((string) $value);
        if ($id !== '' && !in_array($id, $normalized, true)) {
            $normalized[] = $id;
        }
    }

    return $normalized;
}

// ---------------------------------------------------------------------------
// Stage: discover_workloads — find every tests/bench/*.php file.
//
// Bench discovery is intentionally simpler than test discovery: no
// phpunit.xml.dist parsing, no suffix/prefix flexibility. Every PHP file
// under tests/bench/ is a workload. That keeps the workload-author's
// mental model trivial: "drop a file in tests/bench/, return a callable,
// you have a benchmark."
// ---------------------------------------------------------------------------
pg_stage_begin('discover_workloads');
$workload_files = [];
$workload_sources = [];
try {
    $bench_dir = "$plugin_path/tests/bench";
    if (is_dir($bench_dir)) {
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($bench_dir, RecursiveDirectoryIterator::SKIP_DOTS),
            RecursiveIteratorIterator::LEAVES_ONLY
        );
        foreach ($iterator as $file) {
            if ($file->isFile() && $file->getExtension() === 'php') {
                $path = $file->getPathname();
                $workload_files[] = $path;
                $workload_sources[$path] = 'in_tree';
            }
        }
    }

    $extra_workload_list = '{{EXTRA_WORKLOADS_LIST}}';
    $extra_workloads = $extra_workload_list === '' ? [] : explode(':', $extra_workload_list);
    foreach ($extra_workloads as $path) {
        if (!is_string($path) || $path === '') {
            continue;
        }
        if (!is_file($path) || strtolower(pathinfo($path, PATHINFO_EXTENSION)) !== 'php') {
            pg_log("NOTICE: skipping invalid rig bench workload: " . var_export($path, true));
            continue;
        }
        $workload_files[] = $path;
        $workload_sources[$path] = 'rig';
    }

    sort($workload_files);

    $requested_workloads = pg_bench_normalize_workload_filter('{{BENCH_WORKLOADS_JSON}}');
    if (!empty($requested_workloads)) {
        $requested_lookup = array_fill_keys($requested_workloads, true);
        $available_workloads = [];
        $filtered_workload_files = [];

        foreach ($workload_files as $workload_file) {
            $scenario_id = pg_bench_scenario_id(basename($workload_file));
            $available_workloads[] = $scenario_id;
            if (isset($requested_lookup[$scenario_id])) {
                $filtered_workload_files[] = $workload_file;
            }
        }

        if (empty($filtered_workload_files)) {
            throw new RuntimeException(sprintf(
                'bench_workloads matched no workloads. Requested: %s. Available: %s',
                implode(', ', $requested_workloads),
                empty($available_workloads) ? '(none)' : implode(', ', $available_workloads)
            ));
        }

        $workload_files = $filtered_workload_files;
        pg_log('WORKLOAD_FILTER: requested=' . implode(',', $requested_workloads) . ' matched=' . count($workload_files));
    }

    if (empty($workload_files)) {
        pg_log("NO_WORKLOAD_FILES");
    }
    pg_log("DISCOVERY: dir=$bench_dir in_tree=" . count(array_filter($workload_sources, fn($source) => $source === 'in_tree')) . " rig=" . count(array_filter($workload_sources, fn($source) => $source === 'rig')));
    pg_stage_ok('discover_workloads');
} catch (Throwable $e) {
    pg_stage_fail('discover_workloads', $e);
    exit(1);
}

// ---------------------------------------------------------------------------
// Stage: run_workloads — measure each workload N iterations + warmup.
// ---------------------------------------------------------------------------

/**
 * Compute percentile (linear interpolation) over a sorted ascending array.
 *
 * Uses the same definition homeboy core's parser expects (R-7 / Excel-style):
 * given p in [0, 1], the position is p * (N - 1) and the value is the
 * linear interpolation between the floor and ceil indices.
 */
function pg_bench_percentile(array $sorted_ms, float $p): float {
    $n = count($sorted_ms);
    if ($n === 0) {
        return 0.0;
    }
    if ($n === 1) {
        return $sorted_ms[0];
    }
    $rank = $p * ($n - 1);
    $lo = (int) floor($rank);
    $hi = (int) ceil($rank);
    if ($lo === $hi) {
        return $sorted_ms[$lo];
    }
    $frac = $rank - $lo;
    return $sorted_ms[$lo] * (1 - $frac) + $sorted_ms[$hi] * $frac;
}

/**
 * Aggregate a numeric sample set into the flat BenchMetrics key shape.
 *
 * `$suffix` should include any unit (for example `_ms` for duration metrics)
 * and may be empty for unitless workload-provided counters.
 */
function pg_bench_aggregate_metric(array $samples, string $prefix, string $suffix = ''): array {
    sort($samples);
    $count = count($samples);
    $sum = array_sum($samples);
    $mean = $count > 0 ? $sum / $count : 0.0;
    $key_prefix = $prefix === '' ? '' : $prefix . '_';

    return [
        "{$key_prefix}mean{$suffix}" => $mean,
        "{$key_prefix}p50{$suffix}" => pg_bench_percentile($samples, 0.50),
        "{$key_prefix}p95{$suffix}" => pg_bench_percentile($samples, 0.95),
        "{$key_prefix}p99{$suffix}" => pg_bench_percentile($samples, 0.99),
        "{$key_prefix}min{$suffix}" => $count > 0 ? $samples[0] : 0.0,
        "{$key_prefix}max{$suffix}" => $count > 0 ? $samples[$count - 1] : 0.0,
    ];
}

/** Record custom metrics/metadata from a workload return payload. */
function pg_bench_record_workload_result($result, array &$custom_metric_samples, &$latest_metadata): void {
    if (!is_array($result)) {
        return;
    }

    if (array_key_exists('metadata', $result)) {
        $latest_metadata = $result['metadata'];
    }

    if (!isset($result['metrics']) || !is_array($result['metrics'])) {
        return;
    }

    foreach ($result['metrics'] as $metric => $value) {
        if (!is_string($metric) || $metric === '' || !is_numeric($value)) {
            continue;
        }

        $sample = (float) $value;
        if (!is_finite($sample)) {
            continue;
        }

        $custom_metric_samples[$metric][] = $sample;
    }
}

pg_stage_begin('run_workloads');
$scenarios = [];
$iterations_per_workload = (int) '{{ITERATIONS}}';
$warmup_iterations = 1; // Discard first iteration (autoload + OPcache warmup).

if ($iterations_per_workload < 1) {
    $iterations_per_workload = 1;
}

if (HOMEBOY_BENCH_LIST_ONLY) {
    foreach ($workload_files as $workload_file) {
        $basename = basename($workload_file);
        $scenario_id = pg_bench_scenario_id($basename);
        $source = $workload_sources[$workload_file] ?? 'in_tree';
        $relative_file = $source === 'in_tree'
            ? substr($workload_file, strlen($plugin_path) + 1)
            : $workload_file;

        $scenarios[] = [
            'id' => $scenario_id,
            'file' => $relative_file,
            'source' => $source,
            'iterations' => 0,
            'default_iterations' => $iterations_per_workload,
            'tags' => [],
            'metrics' => new stdClass(),
        ];
    }

    file_put_contents("$plugin_path/.pg-bench-results{{RESULT_SUFFIX}}.json", json_encode([
        'component_id' => '{{COMPONENT_ID}}',
        'iterations' => 0,
        'scenarios' => $scenarios,
    ], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));
    pg_log('LIST_ONLY: scenarios=' . count($scenarios));
    exit(0);
}

try {
    foreach ($workload_files as $workload_file) {
        $basename = basename($workload_file);
        $scenario_id = pg_bench_scenario_id($basename);
        // Path relative to the plugin root for the BenchResults envelope.
        $source = $workload_sources[$workload_file] ?? 'in_tree';
        $relative_file = $source === 'in_tree'
            ? substr($workload_file, strlen($plugin_path) + 1)
            : $workload_file;

        pg_log("WORKLOAD_BEGIN: $scenario_id ($basename)");

        // Each workload returns a callable. `require` (not `require_once`)
        // here so re-runs in the same process re-evaluate the file — that
        // keeps the workload author's expectations simple: "every iteration
        // starts where the file's top-level body left off." For Phase 1
        // we pay the parse cost per workload (not per iteration); good
        // enough until measurements show otherwise.
        $callable = require $workload_file;
        if (!is_callable($callable)) {
            pg_log("WORKLOAD_SKIP: $scenario_id (file did not return a callable)");
            continue;
        }

        $timings_ms = [];
        $custom_metric_samples = [];
        $latest_metadata = null;
        $has_metadata = false;
        $peak_memory = 0;

        // Reset PHP's peak-memory counter so the workload's footprint is
        // measured cleanly, not contaminated by previous workloads or
        // bootstrap. memory_reset_peak_usage() requires PHP 8.2+; fall back
        // to silent no-op on older versions (Playground ships 8.3 at
        // wp=6.9, so this should always succeed in the canonical setup).
        if (function_exists('memory_reset_peak_usage')) {
            memory_reset_peak_usage();
        }

        $total_iterations = $iterations_per_workload + $warmup_iterations;
        for ($i = 0; $i < $total_iterations; $i++) {
            $is_warmup = $i < $warmup_iterations;
            $start_ns = hrtime(true);
            $workload_result = $callable();
            $elapsed_ns = hrtime(true) - $start_ns;
            if (!$is_warmup) {
                $timings_ms[] = $elapsed_ns / 1_000_000;
                if (is_array($workload_result) && array_key_exists('metadata', $workload_result)) {
                    $has_metadata = true;
                }
                pg_bench_record_workload_result($workload_result, $custom_metric_samples, $latest_metadata);
            }
        }

        $peak_memory = memory_get_peak_usage(true);

        $metrics = pg_bench_aggregate_metric($timings_ms, '', '_ms');

        ksort($custom_metric_samples);
        foreach ($custom_metric_samples as $metric => $samples) {
            $metrics += pg_bench_aggregate_metric($samples, $metric);
        }

        $scenario = [
            'id' => $scenario_id,
            'file' => $relative_file,
            'source' => $source,
            'iterations' => $iterations_per_workload,
            'metrics' => $metrics,
            'memory' => ['peak_bytes' => $peak_memory],
        ];

        if ($has_metadata) {
            $scenario['metadata'] = $latest_metadata;
        }

        $scenarios[] = $scenario;

        pg_log(sprintf(
            "WORKLOAD_OK: %s p50=%.2fms p95=%.2fms p99=%.2fms",
            $scenario_id,
            pg_bench_percentile($timings_ms, 0.50),
            pg_bench_percentile($timings_ms, 0.95),
            pg_bench_percentile($timings_ms, 0.99)
        ));
    }
    pg_stage_ok('run_workloads');
} catch (Throwable $e) {
    pg_stage_fail('run_workloads', $e);
    exit(1);
}

// ---------------------------------------------------------------------------
// Stage: emit_results — write the BenchResults envelope.
// ---------------------------------------------------------------------------
pg_stage_begin('emit_results');
try {
    // Surface bootstrap stage timings as a synthetic `__bootstrap`
    // scenario (homeboy-extensions#255). Boot is once-per-run (the
    // wp-playground-cli process boots once and runs every workload
    // inside the booted process), so iterations=1 reflects reality —
    // distribution math (p50/p95/p99) doesn't apply to N=1 measurements.
    //
    // Cross-run distribution comes from running the bench harness
    // multiple times (e.g. CI on different commits), not from
    // iterations within a single run. Reusing BenchScenario keeps
    // baseline / regression-detection / cross-rig diff machinery
    // working with no schema change. The `__` prefix on the scenario
    // id makes the synthetic nature obvious in reports.
    $bootstrap_durations = pg_stage_durations_ms();
    $bootstrap_metrics = [];
    foreach (['boot', 'install', 'load_deps', 'load_component'] as $stage) {
        if (isset($bootstrap_durations[$stage])) {
            $bootstrap_metrics["{$stage}_ms"] = $bootstrap_durations[$stage];
        }
    }
    if (!empty($bootstrap_metrics)) {
        array_unshift($scenarios, [
            'id' => '__bootstrap',
            'iterations' => 1,
            'metrics' => $bootstrap_metrics,
        ]);
    }

    $results_path = "$plugin_path/.pg-bench-results{{RESULT_SUFFIX}}.json";
    $envelope = [
        'component_id' => '{{COMPONENT_ID}}',
        'iterations' => $iterations_per_workload,
        'scenarios' => $scenarios,
    ];
    $json = json_encode($envelope, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
    if ($json === false) {
        throw new RuntimeException("json_encode failed: " . json_last_error_msg());
    }
    if (file_put_contents($results_path, $json) === false) {
        throw new RuntimeException("failed to write $results_path");
    }
    pg_log("RESULTS_EMITTED: $results_path (" . count($scenarios) . " scenarios)");
    pg_stage_ok('emit_results');
} catch (Throwable $e) {
    pg_stage_fail('emit_results', $e);
    exit(1);
}

exit(0);
