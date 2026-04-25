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
 * min/max for the BenchResults envelope.
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
pg_run_install_stage(['config_path' => $config_path]);
pg_run_load_deps_stage(['dep_mounts' => '{{PLAYGROUND_DEP_MOUNTS}}']);
pg_run_load_component_stage(['plugin_path' => $plugin_path]);

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
try {
    $bench_dir = "$plugin_path/tests/bench";
    if (!is_dir($bench_dir)) {
        pg_log("NO_WORKLOAD_FILES");
        pg_stage_ok('discover_workloads');
    } else {
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($bench_dir, RecursiveDirectoryIterator::SKIP_DOTS),
            RecursiveIteratorIterator::LEAVES_ONLY
        );
        foreach ($iterator as $file) {
            if ($file->isFile() && $file->getExtension() === 'php') {
                $workload_files[] = $file->getPathname();
            }
        }
        sort($workload_files);
        pg_log("DISCOVERY: dir=$bench_dir found=" . count($workload_files));
        pg_stage_ok('discover_workloads');
    }
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

/** Slugify a workload basename into a scenario id ("BulkImport.php" → "bulk-import"). */
function pg_bench_scenario_id(string $basename): string {
    $name = preg_replace('/\.php$/i', '', $basename);
    $name = preg_replace('/([a-z0-9])([A-Z])/', '$1-$2', $name);
    $name = strtolower($name);
    $name = preg_replace('/[^a-z0-9]+/', '-', $name);
    return trim($name, '-');
}

pg_stage_begin('run_workloads');
$scenarios = [];
$iterations_per_workload = (int) '{{ITERATIONS}}';
$warmup_iterations = 1; // Discard first iteration (autoload + OPcache warmup).

if ($iterations_per_workload < 1) {
    $iterations_per_workload = 1;
}

try {
    foreach ($workload_files as $workload_file) {
        $basename = basename($workload_file);
        $scenario_id = pg_bench_scenario_id($basename);
        // Path relative to the plugin root for the BenchResults envelope.
        $relative_file = substr($workload_file, strlen($plugin_path) + 1);

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
            $callable();
            $elapsed_ns = hrtime(true) - $start_ns;
            if (!$is_warmup) {
                $timings_ms[] = $elapsed_ns / 1_000_000;
            }
        }

        $peak_memory = memory_get_peak_usage(true);

        sort($timings_ms);
        $count = count($timings_ms);
        $sum = array_sum($timings_ms);
        $mean = $count > 0 ? $sum / $count : 0.0;

        $scenarios[] = [
            'id' => $scenario_id,
            'file' => $relative_file,
            'iterations' => $iterations_per_workload,
            'metrics' => [
                'mean_ms' => $mean,
                'p50_ms' => pg_bench_percentile($timings_ms, 0.50),
                'p95_ms' => pg_bench_percentile($timings_ms, 0.95),
                'p99_ms' => pg_bench_percentile($timings_ms, 0.99),
                'min_ms' => $count > 0 ? $timings_ms[0] : 0.0,
                'max_ms' => $count > 0 ? $timings_ms[$count - 1] : 0.0,
            ],
            'memory' => ['peak_bytes' => $peak_memory],
        ];

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
