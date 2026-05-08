<?php
/**
 * Shared Playground bootstrap helpers.
 *
 * Extracted from scripts/test/playground-runner.php so any runner that boots
 * WordPress inside Playground (PHPUnit tests, performance benchmarks, future
 * runner kinds) can reuse the same boot path. A regression in `boot` /
 * `install` / `load_deps` / `load_component` should affect every consumer
 * identically — that only holds if every consumer goes through the same
 * code.
 *
 * Stages provided:
 *   - boot           — render wp-tests-config.php, load composer autoloader
 *   - install        — wp-phpunit install.php (creates WP + tables in-process)
 *   - load_wordpress — wp-load.php for already-installed persisted sites
 *   - load_deps      — load Plugin-Name-headed entry files for declared deps
 *   - load_component — load the plugin/theme under test
 *
 * Stages NOT provided (caller's responsibility):
 *   - load_fixtures  — PHPUnit-specific testcase classes; only test runner needs them
 *   - discover_*     — caller-specific (PHPUnit test discovery vs bench workload discovery)
 *   - load_*         — caller-specific
 *   - run_*          — caller-specific (PHPUnit::run vs bench iteration loop)
 *
 * GLOBAL CONTRACT
 *
 * Callers MUST define the following globals before requiring this file (or
 * the boot path will write to /dev/null and stage fatals will lose
 * attribution):
 *
 *   $result_file   — absolute path the structured log is appended to
 *   $current_stage — string, initial value "preboot" (mutated by pg_stage_begin)
 *
 * The shutdown handler reads $current_stage to attribute fatal errors. The
 * pg_log() / pg_stage_*() helpers append to $result_file. Both are kept as
 * globals (not passed-by-value) so existing callers don't need argument
 * threading at every site — relocation is the goal of this extraction, not
 * an API redesign.
 *
 * STAGE EXECUTORS
 *
 * Each stage executor takes a $cfg array and:
 *   - calls pg_stage_begin($stage)
 *   - performs the stage's work inside try/catch
 *   - calls pg_stage_ok($stage) on success, pg_stage_fail($stage, $e) + exit(1) on Throwable
 *
 * Required $cfg keys are documented per stage. Unknown keys are ignored —
 * callers can pass extra keys that downstream stages need without the
 * upstream stages having to know about them.
 */

// ---------------------------------------------------------------------------
// Diagnostics: structured log helpers + error/shutdown handlers
// ---------------------------------------------------------------------------

/**
 * Append a line to the structured log.
 *
 * Reads the global $result_file the caller defined before requiring this
 * file. Each call appends one line; the bash runners parse the log
 * line-by-line using the prefixes defined in pg_stage_*().
 */
function pg_log($msg) {
    global $result_file;
    file_put_contents($result_file, $msg . "\n", FILE_APPEND);
}

/**
 * Return compact bootstrap context for diagnostics emitted from inside WP hooks.
 */
function pg_diagnostic_context(): string {
    global $current_stage;

    $hook = function_exists('current_filter') ? current_filter() : null;
    if (!is_string($hook) || $hook === '') {
        $hook = 'none';
    }

    $installing = function_exists('wp_installing') && wp_installing() ? 'true' : 'false';

    return 'stage=' . ($current_stage ?: 'unknown') . ' hook=' . $hook . ' wp_installing=' . $installing;
}

/**
 * Module-scope stage timing storage.
 *
 * Stores `hrtime(true)` start times in `_starts_ns` keyed by stage name,
 * and successful-stage durations in `_durations_ms` keyed by stage name
 * (float milliseconds). Stages that begin but never `ok` (i.e. failed)
 * are absent from `_durations_ms` — only successful stages have
 * meaningful timings.
 *
 * Exposed via pg_stage_durations_ms() to the bench runner, which
 * surfaces them as a synthetic `__bootstrap` scenario in the
 * BenchResults envelope (homeboy-extensions#255).
 */
function &pg_stage_timings_ref(): array {
    static $timings = ['_starts_ns' => [], '_durations_ms' => []];
    return $timings;
}

/**
 * Mark the start of a bootstrap stage.
 *
 * Updates the global $current_stage so the shutdown handler can attribute
 * fatal errors to the right stage even when an exit happens mid-stage.
 * Also records `hrtime(true)` for later duration calculation.
 */
function pg_stage_begin($stage) {
    global $current_stage;
    $current_stage = $stage;
    $timings = &pg_stage_timings_ref();
    $timings['_starts_ns'][$stage] = hrtime(true);
    pg_log("STAGE_BEGIN:$stage");
}

/**
 * Mark a stage as completed cleanly.
 *
 * Computes and stores the elapsed duration in milliseconds, but only if
 * a matching pg_stage_begin() was recorded — defensive against any
 * caller emitting a STAGE_OK without a paired BEGIN.
 */
function pg_stage_ok($stage) {
    $timings = &pg_stage_timings_ref();
    if (isset($timings['_starts_ns'][$stage])) {
        $elapsed_ns = hrtime(true) - $timings['_starts_ns'][$stage];
        $timings['_durations_ms'][$stage] = $elapsed_ns / 1_000_000;
    }
    pg_log("STAGE_OK:$stage");
}

/**
 * Return successful-stage durations in milliseconds.
 *
 * Result shape: ['boot' => 1234.56, 'install' => 234.5, ...].
 * Only stages that completed via pg_stage_ok() are included; stages
 * that failed (pg_stage_fail / fatal shutdown) are absent because
 * their timings would be misleading.
 */
function pg_stage_durations_ms(): array {
    $timings = &pg_stage_timings_ref();
    return $timings['_durations_ms'];
}

/**
 * Mark a stage as failed with a caught Throwable. Logs the message + a
 * stack trace; callers typically `exit(1)` immediately after to keep the
 * shutdown handler from layering its own STAGE_FATAL line on top.
 */
function pg_stage_fail($stage, Throwable $e) {
    $msg = get_class($e) . ': ' . $e->getMessage()
        . ' at ' . $e->getFile() . ':' . $e->getLine();
    pg_log("STAGE_FAIL:$stage:$msg");
    pg_log("TRACE:");
    foreach (explode("\n", $e->getTraceAsString()) as $line) {
        pg_log("  $line");
    }
}

/**
 * Install the global error + shutdown handlers used by all Playground
 * runners. Idempotent — safe to call once per runner script.
 *
 * - Warning/notice handler captures non-fatal output to the structured log
 *   so it doesn't vanish into Playground's stderr (which is hard to surface
 *   from the host shell). Returns false so PHP's default handler still runs.
 * - Shutdown handler emits a STAGE_FATAL line with the current stage when
 *   an uncatchable error (E_ERROR / E_PARSE / E_CORE_ERROR / E_COMPILE_ERROR)
 *   ends the script. Without this, the bash runner sees an empty result file
 *   and can't attribute the failure.
 */
function pg_install_diagnostics_handlers() {
    set_error_handler(function ($severity, $message, $file, $line) {
        if (!(error_reporting() & $severity)) {
            return false;
        }
        $label = [
            E_WARNING => 'WARNING',
            E_NOTICE => 'NOTICE',
            E_DEPRECATED => 'DEPRECATED',
            E_USER_WARNING => 'USER_WARNING',
            E_USER_NOTICE => 'USER_NOTICE',
            E_USER_DEPRECATED => 'USER_DEPRECATED',
            E_STRICT => 'STRICT',
        ][$severity] ?? "E_$severity";
        pg_log("NOTICE:$label: $message at $file:$line context=" . pg_diagnostic_context());
        return false;
    });

    register_shutdown_function(function () {
        global $current_stage;
        $error = error_get_last();
        if ($error && in_array($error['type'], [E_ERROR, E_PARSE, E_CORE_ERROR, E_COMPILE_ERROR], true)) {
            pg_log("STAGE_FATAL:$current_stage:{$error['message']} at {$error['file']}:{$error['line']}");
        }
    });
}

/**
 * Snapshot currently-registered callback IDs for a WordPress hook.
 *
 * Runners can load the component before wp-phpunit install so plugins attach
 * callbacks before lazy core registries initialize. Some request-end callbacks
 * registered by that early load are unsafe before activation creates plugin
 * tables, so callers can snapshot a hook before early load and remove only the
 * callbacks added by the component during install.
 */
function pg_snapshot_wordpress_hook_callbacks(string $hook_name): array {
    global $wp_filter;

    $snapshot = [];
    if (!isset($wp_filter[$hook_name]) || !isset($wp_filter[$hook_name]->callbacks)) {
        return $snapshot;
    }

    foreach ($wp_filter[$hook_name]->callbacks as $priority => $callbacks) {
        foreach (array_keys($callbacks) as $callback_id) {
            $snapshot[$priority . ':' . $callback_id] = true;
        }
    }

    return $snapshot;
}

/**
 * Remove callbacks added to a WordPress hook since a prior snapshot.
 */
function pg_remove_new_wordpress_hook_callbacks(string $hook_name, array $before): void {
    global $wp_filter;

    if (!isset($wp_filter[$hook_name]) || !isset($wp_filter[$hook_name]->callbacks)) {
        return;
    }

    foreach ($wp_filter[$hook_name]->callbacks as $priority => $callbacks) {
        foreach (array_keys($callbacks) as $callback_id) {
            if (isset($before[$priority . ':' . $callback_id])) {
                continue;
            }

            unset($wp_filter[$hook_name]->callbacks[$priority][$callback_id]);
        }

        if (empty($wp_filter[$hook_name]->callbacks[$priority])) {
            unset($wp_filter[$hook_name]->callbacks[$priority]);
        }
    }
}

/**
 * Extract and remove callbacks added to a WordPress hook since a snapshot.
 *
 * The returned callback records can be run later with
 * pg_run_deferred_wordpress_hook_callbacks(). This is intentionally for normal
 * lifecycle hooks whose callbacks are unsafe during wp-phpunit install, not for
 * one-shot lazy registries such as the Abilities API hooks.
 */
function pg_defer_new_wordpress_hook_callbacks(string $hook_name, array $before): array {
    global $wp_filter;

    $deferred = [];
    if (!isset($wp_filter[$hook_name]) || !isset($wp_filter[$hook_name]->callbacks)) {
        return $deferred;
    }

    foreach ($wp_filter[$hook_name]->callbacks as $priority => $callbacks) {
        foreach ($callbacks as $callback_id => $callback) {
            if (isset($before[$priority . ':' . $callback_id])) {
                continue;
            }

            $deferred[] = [
                'priority' => (int) $priority,
                'callback' => $callback,
            ];
            unset($wp_filter[$hook_name]->callbacks[$priority][$callback_id]);
        }

        if (empty($wp_filter[$hook_name]->callbacks[$priority])) {
            unset($wp_filter[$hook_name]->callbacks[$priority]);
        }
    }

    usort($deferred, static function (array $left, array $right): int {
        return $left['priority'] <=> $right['priority'];
    });

    return $deferred;
}

/**
 * Run callbacks extracted by pg_defer_new_wordpress_hook_callbacks().
 *
 * When a hook name is supplied, the callbacks run with that hook on
 * $wp_current_filter. This preserves WordPress lifecycle context for APIs that
 * validate doing_action() while still letting the runner defer DB-touching work
 * until after wp-phpunit has created tables.
 */
function pg_run_deferred_wordpress_hook_callbacks(array $deferred, array $args = [], ?string $hook_name = null): void {
    global $wp_current_filter;

    $pushed_hook = false;
    if (is_string($hook_name) && $hook_name !== '') {
        if (!is_array($wp_current_filter)) {
            $wp_current_filter = [];
        }
        $wp_current_filter[] = $hook_name;
        $pushed_hook = true;
    }

    try {
        foreach ($deferred as $entry) {
            $callback = $entry['callback'] ?? null;
            if (!is_array($callback) || !isset($callback['function'])) {
                continue;
            }

            $accepted_args = isset($callback['accepted_args']) ? (int) $callback['accepted_args'] : count($args);
            call_user_func_array($callback['function'], array_slice($args, 0, $accepted_args));
        }
    } finally {
        if ($pushed_hook) {
            array_pop($wp_current_filter);
        }
    }
}

/**
 * Reopen a one-shot WordPress action so callbacks replayed after install can
 * still attach to lazy registries that may have initialized during install.
 */
function pg_reopen_wordpress_action(string $hook_name): bool {
    global $wp_actions;

    if (!function_exists('did_action') || did_action($hook_name) === 0) {
        return false;
    }

    if (!is_array($wp_actions)) {
        $wp_actions = [];
    }

    $wp_actions[$hook_name] = 0;
    pg_log("NOTICE:reopened WordPress action after install: {$hook_name}");

    return true;
}

/**
 * Fire a reopened one-shot WordPress action after deferred plugin callbacks
 * have been replayed.
 */
function pg_fire_reopened_wordpress_action(string $hook_name, bool $reopened): void {
    if (!$reopened || !function_exists('do_action')) {
        return;
    }

    do_action($hook_name);
}

// ---------------------------------------------------------------------------
// Stage executors
// ---------------------------------------------------------------------------

/**
 * Pre-load WP-CLI's namespaced function files before composer autoload-files run.
 *
 * `wp-cli/wp-cli`'s composer autoload is `psr-0` for the `WP_CLI\` namespace
 * plus a small classmap for `WP_CLI` and `WP_CLI_Command`. The namespaced
 * function files (`php/utils.php`, `php/dispatcher.php`) are NOT autoloaded
 * because PSR-0/PSR-4 only resolve classes, not standalone function files.
 *
 * That's normally fine — the standard `wp` phar boot path runs
 * `WP_CLI\bootstrap()` which manually requires those files via the
 * `LoadUtilityFunctions` and `LoadDispatcher` bootstrap steps.
 *
 * It breaks here because the bundled WP-CLI command packages
 * (`wp-cli/extension-command` for `wp plugin`, `wp-cli/entity-command` for
 * `wp post`/`wp option`, etc.) register their commands eagerly via composer
 * autoload `files` entries. Each entry calls `WP_CLI::add_command()` which
 * walks the dispatcher and ultimately calls `WP_CLI\Utils\load_command()`
 * inside `RootCommand::find_subcommand()`. If `php/utils.php` hasn't been
 * required yet, that namespaced function doesn't exist and composer's
 * autoload-files phase fatals with:
 *
 *   Call to undefined function WP_CLI\Utils\load_command()
 *
 * Pre-requiring the two function-namespace files before `vendor/autoload.php`
 * runs makes the bundled command packages' eager registration succeed, so
 * configured workload `wp-cli` steps can use `wp plugin install --activate`,
 * `wp theme install`, `wp option update`, etc — the full bundled command
 * surface a `wp` user gets from the standalone phar (homeboy-extensions#454).
 *
 * Idempotent: each file is gated by a `function_exists` check on a function
 * the file declares, so calling this twice (or before plugins that
 * pre-loaded WP-CLI utilities themselves) is safe.
 */
function pg_preload_wp_cli_namespaced_functions(): void {
    $wp_cli_root = '/homeboy-extension/vendor/wp-cli/wp-cli';

    // WP_CLI_ROOT is referenced inside php/utils.php (load_command() resolves
    // command files relative to it). Define it before requiring utils.php so
    // the bundled command packages' eager registration during composer
    // autoload-files can dispatch through the runner without fataling on the
    // missing constant.
    if (!defined('WP_CLI_ROOT')) {
        define('WP_CLI_ROOT', $wp_cli_root);
    }
    if (!defined('WP_CLI_VENDOR_DIR')) {
        define('WP_CLI_VENDOR_DIR', '/homeboy-extension/vendor');
    }
    if (!defined('WP_CLI_VERSION') && is_readable($wp_cli_root . '/VERSION')) {
        define('WP_CLI_VERSION', trim(file_get_contents($wp_cli_root . '/VERSION')));
    }
    if (!defined('WP_CLI_START_MICROTIME')) {
        define('WP_CLI_START_MICROTIME', microtime(true));
    }

    if (!function_exists('WP_CLI\\Utils\\parse_str_to_argv') && is_readable($wp_cli_root . '/php/utils.php')) {
        require_once $wp_cli_root . '/php/utils.php';
    }
    if (!function_exists('WP_CLI\\Dispatcher\\get_path') && is_readable($wp_cli_root . '/php/dispatcher.php')) {
        require_once $wp_cli_root . '/php/dispatcher.php';
    }
    // utils-wp.php declares additional WP-aware functions in the WP_CLI\Utils
    // namespace (`get_upgrader`, `wp_clean_update_cache`, etc.) used by
    // `wp-cli/extension-command` for plugin/theme install + update commands.
    // Without this, `wp plugin install` fatals on `get_upgrader()` after the
    // dispatcher already accepted the command.
    if (!function_exists('WP_CLI\\Utils\\get_upgrader') && is_readable($wp_cli_root . '/php/utils-wp.php')) {
        require_once $wp_cli_root . '/php/utils-wp.php';
    }
}

/**
 * Run the `boot` stage: render wp-tests-config.php and load composer autoload.
 *
 * Required $cfg keys: (none)
 *
 * Optional $cfg keys:
 *   - extra_defines: associative array of `CONSTANT => value` appended to
 *     wp-tests-config.php as additional `define()` statements. Lets a
 *     component declare its own wp-config-level constants without shipping
 *     a custom drop-in or duplicating boot logic. Values preserve their
 *     PHP type (booleans, integers, nulls, strings) via `var_export()`,
 *     so `["MY_FLAG" => true]` renders as `define('MY_FLAG', true)` and
 *     not `define('MY_FLAG', 'true')`.
 *
 *     The dispatcher (test-runner-playground.sh / bench-runner-playground.sh)
 *     extracts these from the component's settings JSON (the
 *     `wp_config_defines` setting on the wordpress extension) and forwards
 *     them through the runner template via a sed substitution placeholder.
 *
 *     Reserved constant names (DB_NAME, ABSPATH, table_prefix, etc — anything
 *     defined unconditionally in the canonical config above) cannot be
 *     overridden because PHP's `define()` is single-assignment. A component
 *     attempting to override one will get a PHP NOTICE for the duplicate
 *     define and the canonical value wins. Components that need to vary
 *     these should be filing a separate gap, not working around this seam.
 *
 * Side effect: defines $config_path under the caller's scope by writing to
 * /tmp/wp-tests-config.php (the canonical location wp-phpunit's install.php
 * expects). The path is also returned so callers don't have to hard-code it.
 *
 *   - skip_test_config: when true, do not write the wp-phpunit config or
 *     define the canonical test DB constants. Extra defines are applied to
 *     the current process directly so installed-site runs can boot the
 *     persisted site's own wp-config.php without test-config pollution.
 *
 * @return string|null Path to the generated wp-tests-config.php, or null when skipped.
 */
function pg_run_boot_stage(array $cfg = []): ?string {
    pg_stage_begin('boot');
    try {
        $extra_defines = $cfg['extra_defines'] ?? [];
        $skip_test_config = !empty($cfg['skip_test_config']);

        if ($skip_test_config) {
            if (!empty($extra_defines) && is_array($extra_defines)) {
                foreach ($extra_defines as $name => $value) {
                    if (!is_string($name) || !preg_match('/^[A-Z_][A-Z0-9_]*$/i', $name)) {
                        pg_log("NOTICE: skipping invalid wp_config_defines key: " . var_export($name, true));
                        continue;
                    }
                    if (!defined($name)) {
                        define($name, $value);
                    }
                }
            }

            pg_preload_wp_cli_namespaced_functions();
            require_once '/homeboy-extension/vendor/autoload.php';
            pg_stage_ok('boot');
            return null;
        }

        $config_path = '/tmp/wp-tests-config.php';
        $config = <<<'CONFIG'
<?php
$table_prefix = 'wptests_';
define('DB_NAME', ':memory:');
define('DB_USER', 'root');
define('DB_PASSWORD', '');
define('DB_HOST', 'localhost');
define('DB_CHARSET', 'utf8');
define('WP_TESTS_DOMAIN', 'example.org');
define('WP_TESTS_EMAIL', 'admin@example.org');
define('WP_TESTS_TITLE', 'Test Blog');
define('WP_PHP_BINARY', 'php');
define('ABSPATH', '/wordpress/');
define('FS_CHMOD_FILE', 0644);
define('FS_CHMOD_DIR', 0755);
define('FS_METHOD', 'direct');
CONFIG;

        if (!empty($extra_defines) && is_array($extra_defines)) {
            $config .= "\n\n// Component-declared wp_config_defines.\n";
            foreach ($extra_defines as $name => $value) {
                if (!is_string($name) || !preg_match('/^[A-Z_][A-Z0-9_]*$/i', $name)) {
                    pg_log("NOTICE: skipping invalid wp_config_defines key: " . var_export($name, true));
                    continue;
                }
                // var_export preserves PHP type — true/false/int/null/string
                // all round-trip cleanly. Arrays and objects round-trip too,
                // though wp-config rarely needs them.
                $config .= sprintf(
                    "if (!defined('%s')) { define('%s', %s); }\n",
                    $name,
                    $name,
                    var_export($value, true)
                );
            }
        }

        file_put_contents($config_path, $config);

        pg_preload_wp_cli_namespaced_functions();
        require_once '/homeboy-extension/vendor/autoload.php';
        pg_stage_ok('boot');
        return $config_path;
    } catch (Throwable $e) {
        pg_stage_fail('boot', $e);
        exit(1);
    }
}

/**
 * Run the `install` stage: invoke wp-phpunit's install.php in-process.
 *
 * Boots WordPress AND creates the test database tables without spawning a
 * subprocess (which Playground's PHP-WASM doesn't natively support pre-#3481).
 *
 * Required $cfg keys:
 *   - config_path: absolute path to the wp-tests-config.php produced by
 *     pg_run_boot_stage(). Pass the return value of that function.
 *
 * Optional $cfg keys:
 *   - tests_dir: vendor path to wp-phpunit. Defaults to the canonical
 *     '/homeboy-extension/vendor/wp-phpunit/wp-phpunit'.
 */
function pg_run_install_stage(array $cfg) {
    pg_stage_begin('install');
    try {
        $tests_dir = $cfg['tests_dir'] ?? '/homeboy-extension/vendor/wp-phpunit/wp-phpunit';
        $config_path = $cfg['config_path'];
        $argv = ['install.php', $config_path, 'no_ms_tests', 'no_core_tests'];
        $_SERVER['argv'] = $argv;
        require_once "$tests_dir/includes/install.php";
        while (ob_get_level() > 0) {
            @ob_end_clean();
        }
        pg_stage_ok('install');
    } catch (Throwable $e) {
        pg_stage_fail('install', $e);
        exit(1);
    }
}

/**
 * Run the `load_wordpress` stage: boot an already-installed WordPress site.
 *
 * Installed-site bench mode lets Playground prepare `/wordpress` before the
 * runner starts (usually from a shared-state mount), then this stage loads the
 * persisted site's own wp-config.php/wp-settings.php instead of running the
 * wp-phpunit installer again.
 */
function pg_run_load_wordpress_stage(array $cfg = []) {
    pg_stage_begin('load_wordpress');
    try {
        $wp_load_path = $cfg['wp_load_path'] ?? '/wordpress/wp-load.php';
        if (!file_exists($wp_load_path)) {
            throw new RuntimeException("wp-load.php not found at $wp_load_path");
        }
        require_once $wp_load_path;
        while (ob_get_level() > 0) {
            @ob_end_clean();
        }
        pg_stage_ok('load_wordpress');
    } catch (Throwable $e) {
        pg_stage_fail('load_wordpress', $e);
        exit(1);
    }
}

/**
 * Run the `load_deps` stage: require Plugin-Name-headed entry files for
 * every dependency the host runner declared via mount paths.
 *
 * The host bash runner translates HOMEBOY_WORDPRESS_DEPENDENCY_PATHS into
 * mount points like /wordpress/wp-content/plugins/<dep>/, then writes the
 * resulting paths into a newline-separated list which gets sed'd into the
 * runner template at {{PLAYGROUND_DEP_MOUNTS}}. This stage parses that list
 * and loads each dependency's main plugin file.
 *
 * This stage ONLY requires the entry files. Activation hooks are fired later
 * by `pg_run_activation_stage()` after wp-phpunit's install path has created
 * the test tables (homeboy-extensions#431). Firing `activate_<plugin>` here
 * — which is now reachable via the muplugins_loaded callback in the runner
 * templates (post-#427) — would dispatch DB-touching activation callbacks
 * before the wptests_* tables exist and fatal in any plugin whose activation
 * touches the database.
 *
 * Required $cfg keys:
 *   - dep_mounts: newline-separated string of dependency mount paths
 *     inside the Playground VFS. Empty string is fine — no-op.
 *
 * @return string[] Ordered list of dependency plugin entry files that were
 *   require_once'd, suitable for passing to `pg_run_activation_stage()` as
 *   `plugin_files`. Order matches dep_mounts iteration order so dependency
 *   activation hooks fire in the same order WordPress would normally use.
 */
function pg_run_load_deps_stage(array $cfg): array {
    pg_stage_begin('load_deps');
    try {
        $loaded = [];
        $dep_paths = $cfg['dep_mounts'] ?? '';
        if (!empty($dep_paths)) {
            foreach (explode("\n", $dep_paths) as $dep_mount) {
                $dep_mount = trim($dep_mount);
                if (empty($dep_mount)) {
                    continue;
                }
                $dep_files = glob("$dep_mount/*.php") ?: [];
                foreach ($dep_files as $df) {
                    // db.php is a WordPress drop-in, not a plugin entry file.
                    // Dependencies can ship one too, and re-loading it after
                    // wp-settings.php has already loaded the active drop-in
                    // re-runs database side effects.
                    if (basename($df) === 'db.php') {
                        continue;
                    }
                    if (strpos(file_get_contents($df), 'Plugin Name:') !== false) {
                        require_once $df;
                        $loaded[] = $df;
                        break;
                    }
                }
            }
        }
        pg_stage_ok('load_deps');
        return $loaded;
    } catch (Throwable $e) {
        pg_stage_fail('load_deps', $e);
        exit(1);
    }
}

/**
 * Run the `load_component` stage: load the plugin or theme under test.
 *
 * Detects whether the component is a theme (style.css with `Theme Name:`
 * header) or a plugin (any *.php with `Plugin Name:` header) and loads the
 * appropriate entry file. The `db.php` drop-in is explicitly skipped — it's
 * already loaded by wp-settings.php earlier in the request lifecycle and
 * including it again would re-run its side effects.
 *
 * This stage ONLY requires the entry file. Activation hooks are fired later
 * by `pg_run_activation_stage()` after wp-phpunit's install path has created
 * the test tables (homeboy-extensions#431). The legacy `'activate'` key is
 * accepted for backwards compatibility but no longer affects behavior — the
 * canonical split is now load → install → activation, in that order, with
 * activation always coming through `pg_run_activation_stage()`.
 *
 * Required $cfg keys:
 *   - plugin_path: absolute path inside the Playground VFS, typically
 *     '/wordpress/wp-content/plugins/<slug>'.
 *
 * Optional $cfg keys:
 *   - activate: legacy hint, retained for backwards compatibility with smoke
 *     fixtures and external callers. The value is reflected in the
 *     PLUGIN_LOAD_CONTEXT log line (so existing log parsers keep working) but
 *     activation is never dispatched from this stage. Pass the discovered
 *     plugin file to `pg_run_activation_stage()` to fire activation hooks.
 *
 * @return string|null Absolute path to the plugin entry file that was
 *   require_once'd, or null when the component was a theme or no entry
 *   file was discovered. Pass non-null returns to `pg_run_activation_stage()`
 *   along with any dep entry files from `pg_run_load_deps_stage()`.
 */
function pg_run_load_component_stage(array $cfg): ?string {
    pg_stage_begin('load_component');
    try {
        $plugin_path = $cfg['plugin_path'];
        $loaded_file = null;
        $style_css = "$plugin_path/style.css";
        if (file_exists($style_css) && strpos(file_get_contents($style_css), 'Theme Name:') !== false) {
            pg_log("THEME_DETECTED");
            $fn_php = "$plugin_path/functions.php";
            if (file_exists($fn_php)) {
                require_once $fn_php;
            }
        } else {
            $main_files = glob("$plugin_path/*.php") ?: [];
            foreach ($main_files as $mf) {
                // db.php is a WordPress drop-in, not a plugin entry file. It's
                // already been loaded by wp-settings.php earlier in the request
                // lifecycle. Including it again would re-run its side effects
                // (define() warnings, $wpdb re-init) for no reason.
                if (basename($mf) === 'db.php') {
                    continue;
                }
                if (strpos(file_get_contents($mf), 'Plugin Name:') !== false) {
                    pg_log("PLUGIN_DETECTED " . basename($mf));
                    pg_log(
                        'PLUGIN_LOAD_CONTEXT ' . basename($mf)
                        . ' activate=' . ((($cfg['activate'] ?? true) !== false) ? 'true' : 'false')
                        . ' ' . pg_diagnostic_context()
                    );
                    require_once $mf;
                    $loaded_file = $mf;
                    break;
                }
            }
            if ($loaded_file === null) {
                pg_log("NOTICE:no plugin entry file with 'Plugin Name:' header found in $plugin_path");
            }
        }
        pg_stage_ok('load_component');
        return $loaded_file;
    } catch (Throwable $e) {
        pg_stage_fail('load_component', $e);
        exit(1);
    }
}

/**
 * Run the `activation` stage: fire `activate_<plugin>` hooks for every
 * plugin entry file the upstream load stages collected.
 *
 * Decoupling activation from file load lets wp-phpunit's install.php run
 * BEFORE any DB-touching activation callback executes. Pre-#431, both
 * `pg_run_load_deps_stage()` and `pg_run_load_component_stage()` fired
 * activation inline — which since #427 happens during muplugins_loaded,
 * before wp-phpunit creates the wptests_* tables. Activation callbacks
 * doing `add_option`, `get_users`, schema migrations, etc. fataled with
 * "no such table: wptests_*". Now those callbacks see the same DB shape
 * a real WordPress activation does.
 *
 * Activation order is the order callers pass `plugin_files` in; both runner
 * templates pass deps first, then the component-under-test, mirroring
 * WordPress's normal "deps before dependents" ordering. Each entry is
 * activated exactly once per stage call; callers are responsible for not
 * passing the same file twice.
 *
 * Required $cfg keys:
 *   - plugin_files: ordered list of absolute plugin entry file paths to
 *     activate. Empty list is fine — the stage logs and exits cleanly.
 */
function pg_run_activation_stage(array $cfg): void {
    pg_stage_begin('activation');
    try {
        $plugin_files = $cfg['plugin_files'] ?? [];
        if (!is_array($plugin_files)) {
            $plugin_files = [];
        }
        foreach ($plugin_files as $plugin_file) {
            if (!is_string($plugin_file) || $plugin_file === '') {
                continue;
            }
            pg_activate_plugin_file($plugin_file);
        }
        pg_stage_ok('activation');
    } catch (Throwable $e) {
        pg_stage_fail('activation', $e);
        exit(1);
    }
}

/**
 * Run activation hooks for a plugin entry file after loading it.
 *
 * The Playground backend boots WordPress from wp-phpunit and then requires the
 * plugin file directly. Direct loading registers `register_activation_hook()`
 * callbacks, but it does not fire them. Running the corresponding activation
 * action gives plugins the same schema-preparation seam their normal PHPUnit
 * bootstrap relies on without routing through wp-admin redirects.
 */
function pg_activate_plugin_file(string $plugin_file): void {
    if (!function_exists('plugin_basename') || !function_exists('do_action')) {
        pg_log("NOTICE:cannot activate plugin entry before WordPress plugin API is available: $plugin_file");
        return;
    }

    $plugin_basename = plugin_basename($plugin_file);
    pg_log("PLUGIN_ACTIVATE $plugin_basename");
    pg_log("PLUGIN_ACTIVATE_BEGIN $plugin_basename " . pg_diagnostic_context());

    do_action("activate_$plugin_basename", false);
    do_action('activated_plugin', $plugin_basename, false);

    pg_log("PLUGIN_ACTIVATE_OK $plugin_basename " . pg_diagnostic_context());
}

/**
 * Restrict discovered PHPUnit files to HOMEBOY_CHANGED_TEST_FILES.
 *
 * Homeboy core passes changed test files as component-relative paths. The
 * Playground runner discovers tests in the VFS, so compare normalized
 * component-relative paths and keep the broad discovery path as the fallback
 * when no scoped list was supplied.
 */
function pg_filter_changed_test_files(array $test_files, string $changed_files_json, string $plugin_path): array {
    $decoded = json_decode($changed_files_json, true);
    if (!is_array($decoded) || empty($decoded)) {
        return $test_files;
    }

    $wanted = [];
    foreach ($decoded as $entry) {
        if (!is_scalar($entry)) {
            continue;
        }
        $normalized = pg_normalize_changed_test_file((string) $entry, $plugin_path);
        if ($normalized !== '') {
            $wanted[$normalized] = true;
        }
    }

    if (empty($wanted)) {
        pg_log('NOTICE:HOMEBOY_CHANGED_TEST_FILES did not contain usable test paths');
        return [];
    }

    $filtered = [];
    foreach ($test_files as $file) {
        $relative = pg_component_relative_path((string) $file, $plugin_path);
        if (isset($wanted[$relative])) {
            $filtered[] = $file;
        }
    }

    pg_log('SCOPED_TEST_FILES requested=' . count($wanted) . ' matched=' . count($filtered));
    return $filtered;
}

function pg_normalize_changed_test_file(string $path, string $plugin_path): string {
    $path = trim(str_replace('\\', '/', $path));
    if ($path === '') {
        return '';
    }

    return pg_component_relative_path($path, $plugin_path);
}

function pg_component_relative_path(string $path, string $plugin_path): string {
    $path = trim(str_replace('\\', '/', $path));
    $plugin_path = rtrim(str_replace('\\', '/', $plugin_path), '/');

    if (strpos($path, $plugin_path . '/') === 0) {
        $path = substr($path, strlen($plugin_path) + 1);
    } elseif (strpos($path, '/tests/') !== false) {
        $path = substr($path, strpos($path, '/tests/') + 1);
    }

    while (strpos($path, './') === 0) {
        $path = substr($path, 2);
    }

    return ltrim($path, '/');
}
