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
 * Reuses the shared bootstrap stages from
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
 * Config-declared workloads from the `playground_workloads` extension setting
 * run through the same loop after the same Playground bootstrap. Each entry is
 * one scenario with `run` steps. PHP steps execute inside this PHP-WASM process;
 * WP-CLI steps use `WP_CLI::runcommand(..., ['launch' => false])` when WP-CLI
 * is available in-process.
 *
 * The runner times wall-clock around the callable, captures peak memory
 * after, and aggregates per-iteration measurements into p50/p95/p99/mean/
 * min/max for the BenchResults envelope. Workloads may also return
 * `['metrics' => ['rows' => 10], 'metadata' => ['phase' => 'warm']]`;
 * numeric custom metrics are aggregated with the same percentile machinery
 * and the latest metadata/artifacts payloads are attached to the scenario.
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
require_once '{{BENCH_HELPER_PHP}}';

if (!defined('WP_CLI')) {
    define('WP_CLI', true);
}

pg_install_diagnostics_handlers();

// Stages 1-4: shared boot path, identical to test runner.
//
// Component-declared wp-config defines are forwarded as a JSON-encoded
// associative array via the {{WP_CONFIG_DEFINES_JSON}} placeholder. The
// dispatcher reads the `wp_config_defines` setting from the merged
// settings JSON and passes it through; an empty object ("{}") is the
// no-op case. Decode here and hand to pg_run_boot_stage().
$wp_config_defines_raw = base64_decode('{{WP_CONFIG_DEFINES_JSON_B64}}', true);
if (!is_string($wp_config_defines_raw)) {
    $wp_config_defines_raw = '{}';
}
$wp_config_defines = json_decode($wp_config_defines_raw, true);
if (!is_array($wp_config_defines)) {
    $wp_config_defines = [];
}
$bench_site_mode = '{{BENCH_SITE_MODE}}';
$installed_site_mode = $bench_site_mode === 'installed';

$config_path = pg_run_boot_stage([
    'extra_defines' => $wp_config_defines,
    'skip_test_config' => $installed_site_mode,
]);

// Component-declared bench env vars. Host shell env doesn't propagate
// across the wp-playground-cli sandbox boundary, so workloads' getenv()
// calls return false for anything the parent shell set. The dispatcher
// extracts the `bench_env` setting from HOMEBOY_SETTINGS_JSON and the
// runner calls putenv() for each entry here, before workload discovery,
// so getenv() resolves correctly inside workloads.
//
// Empty object is the no-op case — components that don't declare
// bench_env see no behavioural change.
$bench_env_raw = base64_decode('{{BENCH_ENV_JSON_B64}}', true);
if (!is_string($bench_env_raw)) {
    $bench_env_raw = '{}';
}
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

// Load dependencies and the component during canonical WordPress bootstrap,
// not after wp-settings.php has already fired plugins_loaded. Plugins that
// register on plugins_loaded / init / wp_abilities_api_init (e.g. the entire
// Abilities API surface) silently no-op if their entry file is required after
// those hooks have already run. Hook into wp-phpunit's tests_add_filter so the
// dep + component files are loaded inside muplugins_loaded — fired by
// wp-settings.php before plugins_loaded — exactly mirroring the test runner's
// shape (homeboy-extensions#426).
//
// `installed_site_mode` boots the persisted site via wp-load.php and skips
// wp-phpunit install entirely; that path's own active_plugins option is
// authoritative there, so we keep the historical post-load_wordpress dep load
// for that mode. Only the canonical fresh-install path is moved here — which
// is exactly the path the issue identifies as broken.
$tests_dir = '/homeboy-extension/vendor/wp-phpunit/wp-phpunit';
$dep_mounts = '{{PLAYGROUND_DEP_MOUNTS}}';
if ($installed_site_mode) {
    pg_run_load_wordpress_stage();
    $dep_files = pg_run_load_deps_stage(['dep_mounts' => $dep_mounts]);
    $component_file = pg_run_load_component_stage(['plugin_path' => $plugin_path]);

    // Installed-site mode boots a persisted WordPress whose plugin tables
    // already exist, so activation can run immediately — no install boundary
    // to wait on. Activate deps first, then the component (mirrors WordPress's
    // own dep-before-dependent ordering).
    $activation_files = $dep_files;
    if ($component_file !== null) {
        $activation_files[] = $component_file;
    }
    pg_run_activation_stage(['plugin_files' => $activation_files]);
} else {
    // Component-added init callbacks may touch the DB (a plugin's `init` hook
    // doing schema reads, option reads, etc.). wp-phpunit's install path runs
    // wp-settings.php under wp_installing() and tables aren't ready until
    // install.php finishes its work. Snapshot init/shutdown so we can defer
    // component-added init callbacks past the install boundary, matching the
    // test runner's shape exactly.
    $pre_component_init_callbacks = pg_snapshot_wordpress_hook_callbacks('init');
    $pre_component_shutdown_callbacks = pg_snapshot_wordpress_hook_callbacks('shutdown');
    $deferred_install_init_callbacks = [];

    // Capture plugin entry files from the muplugins_loaded callback so the
    // post-install activation stage can replay them in dep-then-component
    // order. The closure mutates the outer-scope arrays via reference; PHP's
    // closure semantics make this the cleanest way to thread state out of
    // the wp-phpunit harness callback.
    $loaded_dep_files = [];
    $loaded_blueprint_plugin_files = [];
    $loaded_component_file = null;

    require_once "$tests_dir/includes/functions.php";
    tests_add_filter('muplugins_loaded', function () use ($plugin_path, $dep_mounts, $pre_component_init_callbacks, &$deferred_install_init_callbacks, &$loaded_dep_files, &$loaded_blueprint_plugin_files, &$loaded_component_file) {
        pg_bench_prepare_wp_cli_runtime();
        $loaded_dep_files = pg_run_load_deps_stage(['dep_mounts' => $dep_mounts]);
        $exclude_roots = [$plugin_path];
        foreach (explode("\n", $dep_mounts) as $dep_mount) {
            $dep_mount = trim($dep_mount);
            if ($dep_mount !== '') {
                $exclude_roots[] = $dep_mount;
            }
        }
        $loaded_blueprint_plugin_files = pg_bench_load_blueprint_plugins_stage($exclude_roots, '{{PLAYGROUND_BLUEPRINT_PLUGIN_SLUGS}}');
        $loaded_component_file = pg_run_load_component_stage(['plugin_path' => $plugin_path, 'activate' => false]);

        // Defer component-added init callbacks until install.php has created
        // the wptests_* tables. Registrations made on plugins_loaded or
        // earlier still fire normally — only DB-touching init runtime work
        // is delayed.
        tests_add_filter('plugins_loaded', function () use ($pre_component_init_callbacks, &$deferred_install_init_callbacks) {
            $deferred_install_init_callbacks = pg_defer_new_wordpress_hook_callbacks('init', $pre_component_init_callbacks);
        }, PHP_INT_MAX);
    });

    pg_run_install_stage(['config_path' => $config_path, 'tests_dir' => $tests_dir]);

    // Suppress shutdown callbacks the component added during install (request-
    // end DB work runs before activation creates plugin tables). Activation is
    // the canonical seam for install-time side effects.
    pg_remove_new_wordpress_hook_callbacks('shutdown', $pre_component_shutdown_callbacks);

    // Fire activation hooks now that wp-phpunit has created the wptests_*
    // tables. Activation order: deps first, then the component-under-test
    // (homeboy-extensions#431). Pre-#431 activation fired inline during
    // muplugins_loaded — before the test tables existed — so any DB-touching
    // activation callback fataled. The split here keeps file load inside the
    // canonical WordPress lifecycle (preserved from #426/#427) while moving
    // the activation dispatch past the install boundary.
    $activation_files = array_merge($loaded_dep_files, $loaded_blueprint_plugin_files);
    if ($loaded_component_file !== null) {
        $activation_files[] = $loaded_component_file;
    }
    pg_run_activation_stage(['plugin_files' => $activation_files]);

    // Replay plugin-added init callbacks after activation. In a normal request,
    // init-time runtime work observes schemas/options prepared by activation.
    pg_run_deferred_wordpress_hook_callbacks($deferred_install_init_callbacks, [], 'init');
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
        $id = homeboy_bench_scenario_id((string) $value);
        if ($id !== '' && !in_array($id, $normalized, true)) {
            $normalized[] = $id;
        }
    }

    return $normalized;
}

/** Decode config-declared Playground workloads. */
function pg_bench_configured_workloads(string $raw): array {
    if ($raw === '' || $raw === 'null') {
        return [];
    }

    $decoded = json_decode($raw, true);
    if (json_last_error() !== JSON_ERROR_NONE) {
        throw new RuntimeException('playground_workloads must be a JSON array');
    }
    if ($decoded === null) {
        return [];
    }
    if (!is_array($decoded) || !pg_bench_is_list($decoded)) {
        throw new RuntimeException('playground_workloads must be a JSON array');
    }

    $workloads = [];
    foreach ($decoded as $index => $workload) {
        if (!is_array($workload)) {
            throw new RuntimeException("playground_workloads[$index] must be an object");
        }

        $raw_id = $workload['id'] ?? $workload['label'] ?? ('configured-' . ($index + 1));
        if (!is_scalar($raw_id)) {
            throw new RuntimeException("playground_workloads[$index].id must be a string");
        }
        $id = homeboy_bench_scenario_id((string) $raw_id);
        if ($id === '') {
            throw new RuntimeException("playground_workloads[$index].id normalized to an empty scenario id");
        }

        $steps = $workload['run'] ?? null;
        if (!is_array($steps) || empty($steps) || !pg_bench_is_list($steps)) {
            throw new RuntimeException("playground_workloads[$index].run must be a non-empty array");
        }

        foreach ($steps as $step_index => $step) {
            if (!is_array($step)) {
                throw new RuntimeException("playground_workloads[$index].run[$step_index] must be an object");
            }
            $type = $step['type'] ?? '';
            if (!in_array($type, ['php', 'wp-cli', 'ability'], true)) {
                throw new RuntimeException("playground_workloads[$index].run[$step_index].type must be 'php', 'wp-cli', or 'ability'");
            }
            if ($type === 'php' && !isset($step['code']) && !isset($step['file'])) {
                throw new RuntimeException("playground_workloads[$index].run[$step_index] php step requires code or file");
            }
            if ($type === 'wp-cli' && !isset($step['command'])) {
                throw new RuntimeException("playground_workloads[$index].run[$step_index] wp-cli step requires command");
            }
            if ($type === 'ability' && !isset($step['ability'])) {
                throw new RuntimeException("playground_workloads[$index].run[$step_index] ability step requires `ability`");
            }
        }

        $workload['id'] = $id;
        $workload['run'] = $steps;
        $workloads[] = $workload;
    }

    return $workloads;
}

function pg_bench_is_list(array $value): bool {
    return $value === [] || array_keys($value) === range(0, count($value) - 1);
}

function pg_bench_json_object($value, string $label): array {
    if ($value === null) {
        return [];
    }
    if ($value === []) {
        return [];
    }
    if (!is_array($value) || pg_bench_is_list($value)) {
        throw new RuntimeException("$label must be an object");
    }

    return $value;
}

function pg_bench_merge_workload_payload($payload, array &$metrics, array &$artifacts, array &$metadata): void {
    if (!is_array($payload)) {
        return;
    }

    foreach (pg_bench_json_object($payload['metrics'] ?? [], 'metrics') as $metric => $value) {
        if (is_string($metric) && $metric !== '' && is_numeric($value) && is_finite((float) $value)) {
            $metrics[$metric] = (float) $value;
        }
    }

    foreach (pg_bench_json_object($payload['artifacts'] ?? [], 'artifacts') as $name => $artifact) {
        if (!is_string($name) || $name === '') {
            continue;
        }
        if (is_string($artifact) && $artifact !== '') {
            $artifacts[$name] = ['path' => $artifact];
            continue;
        }
        if (!is_array($artifact)) {
            continue;
        }
        $path = $artifact['path'] ?? null;
        if (!is_string($path) || $path === '') {
            continue;
        }
        $normalized = ['path' => $path];
        foreach (['kind', 'label', 'type', 'url'] as $field) {
            if (isset($artifact[$field]) && is_string($artifact[$field])) {
                $normalized[$field] = $artifact[$field];
            }
        }
        $artifacts[$name] = $normalized;
    }

    $metadata = array_replace($metadata, pg_bench_json_object($payload['metadata'] ?? [], 'metadata'));
}

function pg_bench_run_php_step(array $step, string $plugin_path) {
    if (isset($step['file'])) {
        if (!is_scalar($step['file'])) {
            throw new RuntimeException('php step file must be a string');
        }
        $file = (string) $step['file'];
        if ($file === '') {
            throw new RuntimeException('php step file must not be empty');
        }
        if ($file[0] !== '/') {
            $file = $plugin_path . '/' . ltrim($file, '/');
        }
        if (!is_file($file)) {
            throw new RuntimeException("php step file not found: $file");
        }
        $result = require $file;
        return is_callable($result) ? $result() : $result;
    }

    if (!is_scalar($step['code'] ?? null)) {
        throw new RuntimeException('php step code must be a string');
    }

    return eval((string) $step['code']);
}

function pg_bench_prepare_wp_cli_runtime(): void {
    if (!class_exists('WP_CLI')) {
        return;
    }

    $wp_cli_root = dirname(dirname((new ReflectionClass('WP_CLI'))->getFileName()));
    if (!defined('WP_CLI_ROOT')) {
        define('WP_CLI_ROOT', $wp_cli_root);
    }
    if (!defined('WP_CLI_VERSION') && is_readable(WP_CLI_ROOT . '/VERSION')) {
        define('WP_CLI_VERSION', trim(file_get_contents(WP_CLI_ROOT . '/VERSION')));
    }
    if (!defined('WP_CLI_START_MICROTIME')) {
        define('WP_CLI_START_MICROTIME', microtime(true));
    }
    if (!defined('WP_CLI_VENDOR_DIR')) {
        if (file_exists(WP_CLI_ROOT . '/vendor/autoload.php')) {
            define('WP_CLI_VENDOR_DIR', WP_CLI_ROOT . '/vendor');
        } elseif (file_exists(dirname(dirname(WP_CLI_ROOT)) . '/autoload.php')) {
            define('WP_CLI_VENDOR_DIR', dirname(dirname(WP_CLI_ROOT)));
        } elseif (file_exists(dirname(WP_CLI_ROOT) . '/vendor/autoload.php')) {
            define('WP_CLI_VENDOR_DIR', dirname(WP_CLI_ROOT) . '/vendor');
        } else {
            define('WP_CLI_VENDOR_DIR', WP_CLI_ROOT . '/vendor');
        }
    }
    if (!function_exists('WP_CLI\\Utils\\parse_str_to_argv') && is_readable(WP_CLI_ROOT . '/php/utils.php')) {
        require_once WP_CLI_ROOT . '/php/utils.php';
    }
    if (!function_exists('WP_CLI\\Dispatcher\\get_path') && is_readable(WP_CLI_ROOT . '/php/dispatcher.php')) {
        require_once WP_CLI_ROOT . '/php/dispatcher.php';
    }

    $runner = WP_CLI::get_runner();
    $runner_reflection = new ReflectionObject($runner);
    if ($runner_reflection->hasProperty('config')) {
        $config_property = $runner_reflection->getProperty('config');
        $config_property->setAccessible(true);
        $config = $config_property->getValue($runner);
        if (!is_array($config)) {
            $config = [];
        }
        $config += [
            'disabled_commands' => [],
            'debug' => false,
            'color' => false,
            'quiet' => false,
            'prompt' => false,
            'require' => [],
            'ssh' => false,
            'http' => false,
            'skip-plugins' => false,
            'skip-themes' => false,
        ];
        $config_property->setValue($runner, $config);
    }
}

function pg_bench_load_blueprint_plugins_stage(array $exclude_roots, string $allowed_slugs_raw): array {
    pg_stage_begin('load_blueprint_plugins');
    try {
        pg_bench_prepare_wp_cli_runtime();

        $plugin_root = '/wordpress/wp-content/plugins';
        $loaded = [];
        $excluded = array_fill_keys(array_map(static fn($path) => rtrim((string) $path, '/'), $exclude_roots), true);
        $allowed_slugs = array_values(array_filter(array_map('trim', explode("\n", $allowed_slugs_raw))));
        if (empty($allowed_slugs)) {
            pg_log('BLUEPRINT_PLUGIN_LOAD_SKIPPED no configured installPlugin targetFolderName values');
            pg_stage_ok('load_blueprint_plugins');
            return [];
        }

        foreach ($allowed_slugs as $plugin_slug) {
            $candidate = rtrim($plugin_root . '/' . trim($plugin_slug, '/'), '/');
            if (isset($excluded[$candidate])) {
                continue;
            }
            if (!is_dir($candidate)) {
                pg_log('NOTICE:blueprint plugin directory not found: ' . $candidate);
                continue;
            }
            foreach ((glob($candidate . '/*.php') ?: []) as $plugin_file) {
                if (strpos(file_get_contents($plugin_file), 'Plugin Name:') === false) {
                    continue;
                }
                require_once $plugin_file;
                $loaded[] = $plugin_file;
                pg_log('BLUEPRINT_PLUGIN_LOADED ' . basename($candidate) . '/' . basename($plugin_file));
                break;
            }
        }

        pg_stage_ok('load_blueprint_plugins');
        return $loaded;
    } catch (Throwable $e) {
        pg_stage_fail('load_blueprint_plugins', $e);
        exit(1);
    }
}

function pg_bench_run_wp_cli_step(array $step) {
    if (!class_exists('WP_CLI') || !method_exists('WP_CLI', 'runcommand')) {
        throw new RuntimeException('wp-cli workload steps require WP_CLI::runcommand() to be available inside the Playground PHP process');
    }
    pg_bench_prepare_wp_cli_runtime();

    if (!is_scalar($step['command'])) {
        throw new RuntimeException('wp-cli step command must be a string');
    }
    $command = trim((string) $step['command']);
    if (strpos($command, 'wp ') === 0) {
        $command = substr($command, 3);
    }
    if ($command === '') {
        throw new RuntimeException('wp-cli step command must not be empty');
    }

    $parse = isset($step['parse']) && is_scalar($step['parse']) ? (string) $step['parse'] : false;
    $result = WP_CLI::runcommand($command, [
        'launch' => false,
        'exit_error' => false,
        'return' => 'all',
        'parse' => false,
    ]);
    $stdout = is_object($result) && isset($result->stdout) ? (string) $result->stdout : '';
    $stderr = is_object($result) && isset($result->stderr) ? (string) $result->stderr : '';
    if (is_object($result) && isset($result->return_code) && (int) $result->return_code !== 0) {
        throw new RuntimeException(
            "wp-cli step failed with exit code {$result->return_code}: "
            . pg_bench_format_wp_cli_failure($stdout, $stderr)
        );
    }
    if ($parse === 'json' && $stdout !== '') {
        $decoded = json_decode($stdout, true);
        if (json_last_error() !== JSON_ERROR_NONE) {
            throw new RuntimeException('wp-cli step stdout was not valid JSON: ' . json_last_error_msg());
        }
        return is_array($decoded) ? $decoded : ['metadata' => ['stdout' => $decoded]];
    }

    return ['metadata' => ['stdout' => $stdout]];
}

function pg_bench_format_wp_cli_failure(string $stdout, string $stderr): string {
    $parts = [];
    $stderr = trim($stderr);
    if ($stderr !== '') {
        $parts[] = 'stderr=' . pg_bench_excerpt($stderr);
    }

    $stdout = trim($stdout);
    if ($stdout !== '') {
        $decoded = json_decode($stdout, true);
        if (json_last_error() === JSON_ERROR_NONE && is_array($decoded)) {
            $summary = pg_bench_summarize_wp_cli_json_failure($decoded);
            if ($summary !== '') {
                $parts[] = 'json=' . $summary;
            }
        }
        $parts[] = 'stdout=' . pg_bench_excerpt($stdout);
    }

    return $parts ? implode('; ', $parts) : 'no stdout/stderr captured';
}

function pg_bench_summarize_wp_cli_json_failure(array $payload): string {
    $summary = [];

    if (isset($payload['quality']) && is_array($payload['quality'])) {
        $quality = $payload['quality'];
        if (array_key_exists('pass', $quality)) {
            $summary[] = 'quality.pass=' . ($quality['pass'] ? 'true' : 'false');
        }
        if (!empty($quality['failure_reasons']) && is_array($quality['failure_reasons'])) {
            $summary[] = 'failure_reasons=' . implode(',', array_map('strval', $quality['failure_reasons']));
        }
        foreach (['fallback_count', 'invalid_block_count', 'content_loss_count'] as $key) {
            if (isset($quality[$key]) && is_scalar($quality[$key])) {
                $summary[] = $key . '=' . (string) $quality[$key];
            }
        }
    }

    if (isset($payload['theme_slug']) && is_scalar($payload['theme_slug'])) {
        $summary[] = 'theme_slug=' . (string) $payload['theme_slug'];
    }

    return implode(', ', $summary);
}

function pg_bench_excerpt(string $value, int $limit = 1200): string {
    $value = preg_replace('/\s+/', ' ', trim($value));
    if (!is_string($value)) {
        return '';
    }
    if (strlen($value) <= $limit) {
        return $value;
    }
    return substr($value, 0, $limit) . '...';
}

function pg_bench_run_ability_step(array $step) {
    if (!function_exists('wp_get_ability')) {
        throw new RuntimeException('ability workload steps require the WordPress Abilities API (wp_get_ability) to be loaded inside the Playground PHP process');
    }

    if (!isset($step['ability']) || !is_scalar($step['ability'])) {
        throw new RuntimeException('ability step requires a string `ability` name');
    }
    $name = trim((string) $step['ability']);
    if ($name === '') {
        throw new RuntimeException('ability step `ability` must not be empty');
    }

    // The Abilities API requires registrations to happen on the canonical
    // init actions. wp-phpunit's install path boots wp-settings.php under
    // WP_INSTALLING, which short-circuits the lazy registry init. Fire both
    // actions once, idempotently, before resolving so plugin-declared
    // categories and abilities land in the registry. Categories must register
    // before abilities (core enforces the dependency).
    if (function_exists('did_action') && function_exists('do_action')) {
        if (!did_action('wp_abilities_api_categories_init')) {
            do_action('wp_abilities_api_categories_init');
        }
        if (!did_action('wp_abilities_api_init')) {
            do_action('wp_abilities_api_init');
        }
    }

    $ability = wp_get_ability($name);
    if (!$ability) {
        throw new RuntimeException("ability not registered: $name");
    }

    $input = $step['input'] ?? [];
    if ($input !== null && !is_array($input)) {
        throw new RuntimeException('ability step `input` must be an object');
    }

    if (isset($step['user'])) {
        if (!function_exists('get_user_by') || !function_exists('wp_set_current_user')) {
            throw new RuntimeException('ability step `user` requires WordPress user functions to be loaded');
        }
        $candidate = $step['user'];
        $user = false;
        if (is_numeric($candidate)) {
            $user = get_user_by('id', (int) $candidate);
        } elseif (is_string($candidate) && $candidate !== '') {
            $user = get_user_by('login', $candidate) ?: get_user_by('email', $candidate) ?: get_user_by('slug', $candidate);
        }
        if (!$user) {
            throw new RuntimeException('ability step user not found: ' . var_export($candidate, true));
        }
        wp_set_current_user($user->ID);
    }

    if (method_exists($ability, 'execute')) {
        $result = $ability->execute($input ?? []);
    } elseif (is_callable($ability)) {
        $result = $ability($input ?? []);
    } else {
        throw new RuntimeException("ability $name is not executable");
    }

    if (function_exists('is_wp_error') && is_wp_error($result)) {
        $message = method_exists($result, 'get_error_message') ? $result->get_error_message() : 'wp_error';
        throw new RuntimeException("ability $name returned WP_Error: $message");
    }

    if (is_array($result) && (isset($result['metrics']) || isset($result['artifacts']) || isset($result['metadata']))) {
        return $result;
    }

    return ['metadata' => ['return' => $result]];
}

function pg_bench_run_configured_workload(array $workload, string $plugin_path): array {
    $metrics = [];
    $artifacts = [];
    $metadata = [];

    pg_bench_merge_workload_payload($workload, $metrics, $artifacts, $metadata);

    foreach ($workload['run'] as $step) {
        $type = $step['type'];
        switch ($type) {
            case 'php':
                $result = pg_bench_run_php_step($step, $plugin_path);
                break;
            case 'wp-cli':
                $result = pg_bench_run_wp_cli_step($step);
                break;
            case 'ability':
                $result = pg_bench_run_ability_step($step);
                break;
            default:
                throw new RuntimeException("unsupported step type: $type");
        }
        pg_bench_merge_workload_payload($result, $metrics, $artifacts, $metadata);
    }

    return [
        'metrics' => $metrics,
        'artifacts' => $artifacts,
        'metadata' => $metadata,
    ];
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
$workloads = [];
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
                $workloads[] = [
                    'kind' => 'file',
                    'id' => homeboy_bench_scenario_id(basename($path)),
                    'file' => $path,
                    'source' => 'in_tree',
                ];
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
        $workloads[] = [
            'kind' => 'file',
            'id' => homeboy_bench_scenario_id(basename($path)),
            'file' => $path,
            'source' => 'rig',
        ];
    }

    $playground_workloads_raw = base64_decode('{{PLAYGROUND_WORKLOADS_JSON_B64}}', true);
    if (!is_string($playground_workloads_raw)) {
        $playground_workloads_raw = '[]';
    }
    foreach (pg_bench_configured_workloads($playground_workloads_raw) as $configured_workload) {
        $workloads[] = [
            'kind' => 'configured',
            'id' => $configured_workload['id'],
            'label' => isset($configured_workload['label']) && is_scalar($configured_workload['label']) ? (string) $configured_workload['label'] : null,
            'workload' => $configured_workload,
            'source' => 'config',
        ];
    }

    usort($workloads, static function (array $left, array $right): int {
        $left_key = $left['kind'] === 'file' ? $left['file'] : ('~' . $left['id']);
        $right_key = $right['kind'] === 'file' ? $right['file'] : ('~' . $right['id']);
        return strcmp($left_key, $right_key);
    });

    $bench_workloads_raw = base64_decode('{{BENCH_WORKLOADS_JSON_B64}}', true);
    if (!is_string($bench_workloads_raw)) {
        $bench_workloads_raw = 'null';
    }
    $requested_workloads = pg_bench_normalize_workload_filter($bench_workloads_raw);
    if (!empty($requested_workloads)) {
        $requested_lookup = array_fill_keys($requested_workloads, true);
        $available_workloads = [];
        $filtered_workloads = [];

        foreach ($workloads as $workload) {
            $scenario_id = $workload['id'];
            $available_workloads[] = $scenario_id;
            if (isset($requested_lookup[$scenario_id])) {
                $filtered_workloads[] = $workload;
            }
        }

        if (empty($filtered_workloads)) {
            throw new RuntimeException(sprintf(
                'bench_workloads matched no workloads. Requested: %s. Available: %s',
                implode(', ', $requested_workloads),
                empty($available_workloads) ? '(none)' : implode(', ', $available_workloads)
            ));
        }

        $workloads = $filtered_workloads;
        pg_log('WORKLOAD_FILTER: requested=' . implode(',', $requested_workloads) . ' matched=' . count($workloads));
    }

    if (empty($workloads)) {
        pg_log("NO_WORKLOAD_FILES");
    }
    pg_log("DISCOVERY: dir=$bench_dir in_tree=" . count(array_filter($workloads, fn($workload) => $workload['source'] === 'in_tree')) . " rig=" . count(array_filter($workloads, fn($workload) => $workload['source'] === 'rig')) . " config=" . count(array_filter($workloads, fn($workload) => $workload['source'] === 'config')));
    pg_stage_ok('discover_workloads');
} catch (Throwable $e) {
    pg_stage_fail('discover_workloads', $e);
    exit(1);
}

// ---------------------------------------------------------------------------
// Stage: run_workloads — measure each workload N iterations + warmup.
// ---------------------------------------------------------------------------

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
        "{$key_prefix}p50{$suffix}" => homeboy_bench_percentile($samples, 0.50),
        "{$key_prefix}p95{$suffix}" => homeboy_bench_percentile($samples, 0.95),
        "{$key_prefix}p99{$suffix}" => homeboy_bench_percentile($samples, 0.99),
        "{$key_prefix}min{$suffix}" => $count > 0 ? $samples[0] : 0.0,
        "{$key_prefix}max{$suffix}" => $count > 0 ? $samples[$count - 1] : 0.0,
    ];
}

/** Record custom metrics/metadata from a workload return payload. */
function pg_bench_record_workload_result($result, array &$custom_metric_samples, &$latest_metadata, &$latest_artifacts): void {
    if (!is_array($result)) {
        return;
    }

    if (array_key_exists('metadata', $result)) {
        $latest_metadata = $result['metadata'];
    }

    if (array_key_exists('artifacts', $result)) {
        $latest_artifacts = $result['artifacts'];
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
    foreach ($workloads as $workload) {
        $source = $workload['source'];
        $relative_file = $workload['kind'] === 'file'
            ? ($source === 'in_tree' ? substr($workload['file'], strlen($plugin_path) + 1) : $workload['file'])
            : null;

        $scenario = [
            'id' => $workload['id'],
            'source' => $source,
            'iterations' => 0,
            'default_iterations' => $iterations_per_workload,
            'tags' => [],
            'metrics' => new stdClass(),
        ];
        if ($relative_file !== null) {
            $scenario['file'] = $relative_file;
        }
        if (isset($workload['label']) && $workload['label'] !== null) {
            $scenario['metadata'] = ['label' => $workload['label']];
        }
        $scenarios[] = $scenario;
    }

    homeboy_write_bench_results("$plugin_path/.pg-bench-results{{RESULT_SUFFIX}}.json", '{{COMPONENT_ID}}', 0, $scenarios);
    pg_log('LIST_ONLY: scenarios=' . count($scenarios));
    exit(0);
}

try {
    foreach ($workloads as $workload) {
        $scenario_id = $workload['id'];
        // Path relative to the plugin root for the BenchResults envelope.
        $source = $workload['source'];
        $relative_file = $workload['kind'] === 'file'
            ? ($source === 'in_tree' ? substr($workload['file'], strlen($plugin_path) + 1) : $workload['file'])
            : null;

        pg_log("WORKLOAD_BEGIN: $scenario_id (" . ($relative_file ?? 'configured') . ")");

        if ($workload['kind'] === 'file') {
            // Each workload returns a callable. `require` (not `require_once`)
            // here so re-runs in the same process re-evaluate the file.
            $callable = require $workload['file'];
            if (!is_callable($callable)) {
                pg_log("WORKLOAD_SKIP: $scenario_id (file did not return a callable)");
                continue;
            }
        } else {
            $callable = static function () use ($workload, $plugin_path): array {
                return pg_bench_run_configured_workload($workload['workload'], $plugin_path);
            };
        }

        $timings_ms = [];
        $custom_metric_samples = [];
        $latest_metadata = null;
        $latest_artifacts = null;
        $has_metadata = false;
        $has_artifacts = false;
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
                if (is_array($workload_result) && array_key_exists('artifacts', $workload_result)) {
                    $has_artifacts = true;
                }
                pg_bench_record_workload_result($workload_result, $custom_metric_samples, $latest_metadata, $latest_artifacts);
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
            'source' => $source,
            'iterations' => $iterations_per_workload,
            'metrics' => $metrics,
            'memory' => ['peak_bytes' => $peak_memory],
        ];
        if ($relative_file !== null) {
            $scenario['file'] = $relative_file;
        }

        if ($has_metadata && is_array($latest_metadata) && !empty($latest_metadata)) {
            $scenario['metadata'] = $latest_metadata;
        }
        if ($has_artifacts && is_array($latest_artifacts) && !empty($latest_artifacts)) {
            $scenario['artifacts'] = $latest_artifacts;
        }

        $scenarios[] = $scenario;

        pg_log(sprintf(
            "WORKLOAD_OK: %s p50=%.2fms p95=%.2fms p99=%.2fms",
            $scenario_id,
            homeboy_bench_percentile($timings_ms, 0.50),
            homeboy_bench_percentile($timings_ms, 0.95),
            homeboy_bench_percentile($timings_ms, 0.99)
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
    foreach (['boot', 'install', 'load_wordpress', 'load_deps', 'load_component', 'activation'] as $stage) {
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
    homeboy_write_bench_results($results_path, '{{COMPONENT_ID}}', $iterations_per_workload, $scenarios);
    pg_log("RESULTS_EMITTED: $results_path (" . count($scenarios) . " scenarios)");
    pg_stage_ok('emit_results');
} catch (Throwable $e) {
    pg_stage_fail('emit_results', $e);
    exit(1);
}

exit(0);
