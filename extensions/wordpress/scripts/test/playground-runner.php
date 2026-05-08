<?php
/**
 * Playground test runner template.
 *
 * This file is rendered by scripts/test/test-runner-playground.sh via sed
 * substitution of {{PLUGIN_SLUG}} and {{PLAYGROUND_DEP_MOUNTS}} before being
 * mounted into the Playground VFS as /runner.php.
 *
 * DIAGNOSTICS CONTRACT
 *
 * The result file (/wordpress/wp-content/plugins/<slug>/.pg-test-result.txt)
 * is the structured log the host-side bash runner parses. Every line takes
 * one of these forms:
 *
 *   STAGE_BEGIN:<stage>          - entering a bootstrap phase
 *   STAGE_OK:<stage>             - phase completed cleanly
 *   STAGE_FAIL:<stage>:<msg>     - caught Throwable during phase
 *   STAGE_FATAL:<stage>:<msg>    - uncatchable fatal (from shutdown handler)
 *   NOTICE:<msg>                 - PHP warning/notice that escaped silencing
 *   PLUGIN_DETECTED <basename>   - loaded plugin entry file
 *   PLUGIN_LOAD_CONTEXT <...>    - load stage/hook/installing/activation context
 *   PLUGIN_ACTIVATE_BEGIN <...>  - post-install activation hook dispatch begins
 *   PLUGIN_ACTIVATE_OK <...>     - post-install activation hook dispatch ended
 *   THEME_DETECTED               - loaded theme (style.css + functions.php)
 *   NO_TEST_FILES                - discovery found no candidates
 *   RUNNING <n> TEST FILES       - starting PHPUnit
 *   ALL TESTS PASSED             - result.wasSuccessful() == true
 *   SOME TESTS FAILED            - result.wasSuccessful() == false
 *   TESTS: <n> FAILURES: <n> ERRORS: <n>
 *
 * Stages (in execution order):
 *   boot             - composer autoload + config write
 *   load_deps        - dependency plugin entry files (fired inside muplugins_loaded)
 *   load_component   - plugin/theme under test (fired inside muplugins_loaded)
 *   install          - wp-phpunit install.php (creates WP + tables)
 *   activation       - fire activate_<plugin> hooks for deps + component
 *                      (homeboy-extensions#431 — runs AFTER install so DB-
 *                      touching activation hooks see real tables)
 *   load_fixtures    - test case classes, mock mailer, harness filters
 *   discover_tests   - glob test files, then apply changed-file scope
 *   load_tests       - require_once each test file
 *   run_tests        - PHPUnit execution
 *
 * The bash runner's job: if playground_exit != 0 AND no STAGE_FAIL/FATAL/
 * SOME TESTS FAILED line exists, surface the raw stdout/stderr — something
 * crashed before we could even write to the result file.
 *
 * Plugin lifecycle contract: plugin files are loaded during WordPress bootstrap
 * with activation disabled, while wp_installing() may still be true and test DB
 * tables may not be ready. Runtime callbacks must guard install-time table
 * access. Activation hooks are dispatched later by the runner after the
 * wp-phpunit install stage has created the test tables.
 */

// Report everything. Warnings/notices inside WP bootstrap have historically
// been the silent failure mode — the old ~E_WARNING mask hid real problems.
error_reporting(E_ALL);
ini_set('display_errors', '1');
ini_set('display_startup_errors', '1');

$result_file = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}/.pg-test-result.txt';
$current_stage = 'preboot';

// pg_log / pg_stage_* helpers + the diagnostics handlers + the four shared
// bootstrap stages (boot, install, load_deps, load_component) live in the
// extracted lib so the bench runner can reuse the same boot path and any
// future runner kind inherits it for free. Every consumer reading the same
// `boot` and `install` stage code is the only honest way to compare timings
// across runners (a regression in install must be visible to all of them).
require_once '/homeboy-extension/scripts/lib/playground-bootstrap.php';

pg_install_diagnostics_handlers();

$tests_dir = '/homeboy-extension/vendor/wp-phpunit/wp-phpunit';
$plugin_path = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}';
$selected_test_file = base64_decode('{{PHPUNIT_TEST_FILE_B64}}', true);
if (!is_string($selected_test_file)) {
    $selected_test_file = '';
}

// Stage: boot — render wp-tests-config.php + load composer autoload.
//
// Component-declared wp-config defines flow through the
// {{WP_CONFIG_DEFINES_JSON}} placeholder. Empty object is the no-op
// default for components that don't need the seam.
$wp_config_defines_raw = base64_decode('{{WP_CONFIG_DEFINES_JSON_B64}}', true);
if (!is_string($wp_config_defines_raw)) {
    $wp_config_defines_raw = '{}';
}
$wp_config_defines = json_decode($wp_config_defines_raw, true);
if (!is_array($wp_config_defines)) {
    $wp_config_defines = [];
}
$config_path = pg_run_boot_stage(['extra_defines' => $wp_config_defines]);

// Component-declared bench env vars (used by both bench and test runners
// — name kept consistent across both runner kinds for symmetry). Host
// shell env doesn't cross the wp-playground-cli sandbox boundary; the
// dispatcher extracts `bench_env` from HOMEBOY_SETTINGS_JSON and we
// putenv() each entry here before test fixtures load. Empty object is
// the no-op case.
$bench_env_raw = base64_decode('{{BENCH_ENV_JSON_B64}}', true);
if (!is_string($bench_env_raw)) {
    $bench_env_raw = '{}';
}
$bench_env = json_decode($bench_env_raw, true);
if (is_array($bench_env)) {
    foreach ($bench_env as $name => $value) {
        if (is_string($name) && preg_match('/^[A-Za-z_][A-Za-z0-9_]*$/', $name)) {
            $string_value = is_scalar($value) ? (string) $value : json_encode($value);
            putenv($name . '=' . $string_value);
            $_ENV[$name] = $string_value;
        } else {
            pg_log("NOTICE: skipping invalid bench_env key: " . var_export($name, true));
        }
    }
}

// Load the component during WordPress bootstrap, not after wp-settings.php has
// finished. Plugins that register hooks for core bootstrap events (for example
// wp_abilities_api_init) need to be present before those events fire.
$pre_component_plugins_loaded_callbacks = pg_snapshot_wordpress_hook_callbacks('plugins_loaded');
$pre_component_init_callbacks = pg_snapshot_wordpress_hook_callbacks('init');
$pre_component_shutdown_callbacks = pg_snapshot_wordpress_hook_callbacks('shutdown');
$deferred_install_plugins_loaded_callbacks = [];
$deferred_install_init_callbacks = [];

// Capture plugin entry files from the muplugins_loaded callback so the
// post-install activation stage can replay them in dep-then-component order.
// The closure mutates the outer-scope variables via reference.
$loaded_dep_files = [];
$loaded_component_file = null;

require_once "$tests_dir/includes/functions.php";
tests_add_filter('muplugins_loaded', function () use ($plugin_path, $pre_component_plugins_loaded_callbacks, $pre_component_init_callbacks, &$deferred_install_plugins_loaded_callbacks, &$deferred_install_init_callbacks, &$loaded_dep_files, &$loaded_component_file) {
    $loaded_dep_files = pg_run_load_deps_stage(['dep_mounts' => '{{PLAYGROUND_DEP_MOUNTS}}']);
    $loaded_component_file = pg_run_load_component_stage(['plugin_path' => $plugin_path, 'activate' => false]);

    // Let plugins attach early-bootstrap callbacks before lazy core registries
    // initialize, but defer component-added runtime callbacks until install.php
    // has created the wptests_* tables and activation has prepared plugin-owned
    // state. Defer immediately: waiting for PHP_INT_MAX on plugins_loaded is
    // too late because lower-priority callbacks have already run.
    $deferred_install_plugins_loaded_callbacks = pg_defer_new_wordpress_hook_callbacks('plugins_loaded', $pre_component_plugins_loaded_callbacks);
    $deferred_install_init_callbacks = pg_defer_new_wordpress_hook_callbacks('init', $pre_component_init_callbacks);
});

// Stage: install — wp-phpunit install.php creates WP tables in-process.
pg_run_install_stage(['config_path' => $config_path, 'tests_dir' => $tests_dir]);

// The early component load above preserves core bootstrap ordering for lazy
// registries, but request-end callbacks added during wp-phpunit install can do
// runtime database work before activation has created plugin tables. Suppress
// only callbacks the component added during that install bootstrap; activation
// below remains the post-table seam for install-time side effects.
pg_remove_new_wordpress_hook_callbacks('shutdown', $pre_component_shutdown_callbacks);

// Fire activation hooks now that wp-phpunit has created database tables.
// Pre-#431 activation fired inline during muplugins_loaded — before the test
// tables existed — so any DB-touching activation callback fataled. The split
// here keeps file load inside the canonical WordPress lifecycle (preserved
// from #426/#427) while moving the activation dispatch past the install
// boundary. Activation order: deps first, then the component-under-test,
// mirroring WordPress's normal "deps before dependents" ordering.
$activation_files = $loaded_dep_files;
if ($loaded_component_file !== null) {
    $activation_files[] = $loaded_component_file;
}
pg_run_activation_stage(['plugin_files' => $activation_files]);

// Replay plugin-added runtime callbacks after activation. In a normal
// activated-plugin request, these callbacks observe schemas/options prepared by
// activation.
$pre_replayed_plugins_loaded_init_callbacks = pg_snapshot_wordpress_hook_callbacks('init');
pg_run_deferred_wordpress_hook_callbacks($deferred_install_plugins_loaded_callbacks, [], 'plugins_loaded');
$deferred_install_init_callbacks = array_merge(
    $deferred_install_init_callbacks,
    pg_defer_new_wordpress_hook_callbacks('init', $pre_replayed_plugins_loaded_init_callbacks)
);
usort($deferred_install_init_callbacks, static function (array $left, array $right): int {
    return ($left['priority'] ?? 10) <=> ($right['priority'] ?? 10);
});
pg_run_deferred_wordpress_hook_callbacks($deferred_install_init_callbacks, [], 'init');

// ---------------------------------------------------------------------------
// Stage: load_fixtures (test case classes, mock mailer, harness filters)
//
// Stays inline — PHPUnit-specific. Other runner kinds (bench, future
// integration runners) don't need wp-phpunit's testcase scaffolding.
// ---------------------------------------------------------------------------
pg_stage_begin('load_fixtures');
try {
    global $phpmailer;
    require_once "$tests_dir/includes/mock-mailer.php";
    $phpmailer = new MockPHPMailer(true);

    require_once "$tests_dir/includes/functions.php";
    $GLOBALS['_wp_die_disabled'] = false;
    tests_add_filter('wp_die_handler', '_wp_die_handler_filter');
    tests_add_filter('wp_rest_server_class', '_wp_rest_server_class_filter');
    tests_add_filter('async_update_translation', '__return_false');
    tests_add_filter('automatic_updater_disabled', '__return_true');

    require_once "$tests_dir/includes/phpunit6/compat.php";
    require_once "$tests_dir/includes/phpunit-adapter-testcase.php";
    require_once "$tests_dir/includes/abstract-testcase.php";
    require_once "$tests_dir/includes/testcase.php";
    require_once "$tests_dir/includes/testcase-rest-api.php";
    require_once "$tests_dir/includes/testcase-rest-controller.php";
    require_once "$tests_dir/includes/testcase-rest-post-type-controller.php";
    require_once "$tests_dir/includes/testcase-xmlrpc.php";
    require_once "$tests_dir/includes/testcase-ajax.php";
    require_once "$tests_dir/includes/testcase-canonical.php";
    require_once "$tests_dir/includes/testcase-xml.php";
    require_once "$tests_dir/includes/exceptions.php";
    require_once "$tests_dir/includes/utils.php";
    require_once "$tests_dir/includes/spy-rest-server.php";
    require_once "$tests_dir/includes/class-wp-rest-test-search-handler.php";
    require_once "$tests_dir/includes/class-wp-rest-test-configurable-controller.php";
    require_once "$tests_dir/includes/class-wp-fake-block-type.php";
    require_once "$tests_dir/includes/class-wp-fake-hasher.php";
    require_once "$tests_dir/includes/class-wp-sitemaps-test-provider.php";
    require_once "$tests_dir/includes/class-wp-sitemaps-empty-test-provider.php";
    require_once "$tests_dir/includes/class-wp-sitemaps-large-test-provider.php";

    pg_stage_ok('load_fixtures');
} catch (Throwable $e) {
    pg_stage_fail('load_fixtures', $e);
    exit(1);
}

// ---------------------------------------------------------------------------
// Stage: discover_tests
//
// Recursively scans $plugin_path/tests for files matching the configured
// suffix/prefix patterns. This mirrors PHPUnit's own behavior when given a
// <directory> element in phpunit.xml.dist — it recurses and applies the
// suffix/prefix filters from the element's attributes.
//
// Configuration source priority:
//   1. Extension's /homeboy-extension/phpunit.xml.dist, if readable and parseable
//      for component test directories
//   2. Hardcoded defaults: suffix=Test.php, prefix=test- (covers both PHPUnit
//      native + WordPress conventions so projects don't have to choose)
//
// This is deliberately narrower than parsing the full PHPUnit XML schema.
// The extension's XML is simple and static; we only consume the pieces that
// affect test discovery (directories, suffixes, prefixes, excludes). Bootstrap,
// coverage, env vars etc. don't translate from host filesystem to VFS anyway.
// ---------------------------------------------------------------------------
pg_stage_begin('discover_tests');
try {
    $test_dir = "$plugin_path/tests";
    if (!is_dir($test_dir)) {
        pg_log("NO_TEST_FILES");
        pg_log("NOTICE:tests directory not found at $test_dir");
        pg_stage_ok('discover_tests');
        exit(1);
    }

    [$directories, $suffixes, $prefixes, $excludes] = pg_parse_phpunit_config(
        '/homeboy-extension/phpunit.xml.dist',
        $test_dir
    );

    $test_files = pg_discover_tests($directories, $suffixes, $prefixes, $excludes);
    $changed_test_files_raw = base64_decode('{{CHANGED_TEST_FILES_JSON_B64}}', true);
    if (!is_string($changed_test_files_raw)) {
        $changed_test_files_raw = '[]';
    }
    $test_files = pg_filter_changed_test_files($test_files, $changed_test_files_raw, $plugin_path);

    if ($selected_test_file !== '') {
        $selected_abs = $plugin_path . '/' . ltrim($selected_test_file, '/');
        if (!in_array($selected_abs, $test_files, true)) {
            pg_log("NO_TEST_FILES");
            pg_log("NOTICE:requested PHPUnit test file not discovered: $selected_test_file");
            pg_stage_ok('discover_tests');
            exit(1);
        }
        $test_files = [$selected_abs];
    }

    pg_log("DISCOVERY: dirs=" . implode(',', $directories)
        . " suffixes=" . implode(',', $suffixes)
        . " prefixes=" . implode(',', $prefixes)
        . " excludes=" . count($excludes)
        . " found=" . count($test_files));

    if (empty($test_files)) {
        pg_log("NO_TEST_FILES");
        pg_stage_ok('discover_tests');
        exit(1);
    }
    pg_stage_ok('discover_tests');
} catch (Throwable $e) {
    pg_stage_fail('discover_tests', $e);
    exit(1);
}

/**
 * Parse the extension's phpunit.xml.dist for test-discovery directives.
 *
 * Returns [directories, suffixes, prefixes, excludes]. Absolute paths only:
 * relative `<directory>` entries under tests/ are resolved against the
 * component under test. Extension-internal suites (for example
 * HomeboyWordPress/Tests) are ignored so extension self-tests do not leak into
 * every component run.
 *
 * Falls back to sensible defaults if the XML is missing or unparseable.
 * Logs the reason to NOTICE so users can see which path was taken.
 *
 * @return array{0: string[], 1: string[], 2: string[], 3: string[]}
 */
function pg_parse_phpunit_config($xml_path, $test_dir_default) {
    $directories = [$test_dir_default];
    $suffixes = ['Test.php'];
    $prefixes = ['test-'];
    $excludes = [];

    if (!is_readable($xml_path)) {
        pg_log("NOTICE:phpunit.xml.dist not readable at $xml_path; using defaults");
        return [$directories, $suffixes, $prefixes, $excludes];
    }

    // libxml_use_internal_errors + simplexml so a malformed XML doesn't
    // explode inside the runner — fall back to defaults and keep going.
    $prev_internal_errors = libxml_use_internal_errors(true);
    $xml = @simplexml_load_file($xml_path);
    $load_errors = libxml_get_errors();
    libxml_clear_errors();
    libxml_use_internal_errors($prev_internal_errors);

    if ($xml === false) {
        $first_err = $load_errors ? trim($load_errors[0]->message) : 'unknown';
        pg_log("NOTICE:phpunit.xml.dist parse failed ($first_err); using defaults");
        return [$directories, $suffixes, $prefixes, $excludes];
    }

    $config_dirs = [];
    $config_suffixes = [];
    $config_prefixes = [];
    $plugin_base = dirname($test_dir_default); // plugin root

    // <testsuites><testsuite><directory suffix="..." prefix="...">path</directory>
    foreach ($xml->xpath('//testsuite/directory') ?: [] as $dir) {
        $raw_path = trim((string) $dir);
        if ($raw_path === '') {
            continue;
        }
        if (!pg_is_component_phpunit_directory($raw_path)) {
            continue;
        }
        // Resolve relative to the plugin root (PHPUnit's own convention is
        // "relative to the config file", but we're using the extension's
        // config against a different plugin, so "plugin root" is the
        // semantically correct base here).
        $abs = $raw_path[0] === '/' ? $raw_path : "$plugin_base/$raw_path";
        $config_dirs[] = rtrim($abs, '/');

        // Optional suffix/prefix attributes (comma-separated per PHPUnit docs,
        // though the native schema only supports single values — we accept
        // both for ergonomics).
        if (isset($dir['suffix'])) {
            foreach (explode(',', (string) $dir['suffix']) as $s) {
                $s = trim($s);
                if ($s !== '') {
                    $config_suffixes[] = $s;
                }
            }
        }
        if (isset($dir['prefix'])) {
            foreach (explode(',', (string) $dir['prefix']) as $p) {
                $p = trim($p);
                if ($p !== '') {
                    $config_prefixes[] = $p;
                }
            }
        }
    }

    // <testsuites><testsuite><exclude>path</exclude>
    foreach ($xml->xpath('//testsuite/exclude') ?: [] as $ex) {
        $raw = trim((string) $ex);
        if ($raw === '') {
            continue;
        }
        $abs = $raw[0] === '/' ? $raw : "$plugin_base/$raw";
        $excludes[] = rtrim($abs, '/');
    }

    if (!empty($config_dirs)) {
        $directories = $config_dirs;
        pg_log("NOTICE:phpunit.xml.dist loaded from $xml_path");
    }
    if (!empty($config_suffixes)) {
        $suffixes = $config_suffixes;
    }
    if (!empty($config_prefixes)) {
        $prefixes = $config_prefixes;
    }

    return [$directories, $suffixes, $prefixes, $excludes];
}

/**
 * Whether a phpunit.xml `<directory>` entry belongs to the component under test.
 */
function pg_is_component_phpunit_directory($raw_path) {
    $normalized = str_replace('\\', '/', trim($raw_path));
    $normalized = trim($normalized, '/');

    return $normalized === 'tests' || strpos($normalized, 'tests/') === 0;
}

/**
 * Recursively find test files under the given directories.
 *
 * A file matches if its basename ends with any of the $suffixes OR starts
 * with any of the $prefixes. Files under any path in $excludes are skipped.
 *
 * Uses SplFileInfo + RecursiveIteratorIterator so nested structures like
 * tests/Unit/FooTest.php and tests/Integration/BarTest.php are picked up,
 * which was the Phase 1 gap this discovery rewrite fixes.
 */
function pg_discover_tests(array $directories, array $suffixes, array $prefixes, array $excludes) {
    $found = [];
    foreach ($directories as $dir) {
        if (!is_dir($dir)) {
            pg_log("NOTICE:test directory does not exist: $dir");
            continue;
        }
        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($dir, RecursiveDirectoryIterator::SKIP_DOTS),
            RecursiveIteratorIterator::LEAVES_ONLY
        );
        foreach ($iterator as $file) {
            if (!$file->isFile()) {
                continue;
            }
            $path = $file->getPathname();
            if ($file->getExtension() !== 'php') {
                continue;
            }
            // Exclude paths: any directory prefix match disqualifies.
            foreach ($excludes as $ex) {
                if (strpos($path, $ex) === 0) {
                    continue 2;
                }
            }
            $base = $file->getBasename();
            $matches = false;
            foreach ($suffixes as $s) {
                if ($s !== '' && substr($base, -strlen($s)) === $s) {
                    $matches = true;
                    break;
                }
            }
            if (!$matches) {
                foreach ($prefixes as $p) {
                    if ($p !== '' && strpos($base, $p) === 0) {
                        $matches = true;
                        break;
                    }
                }
            }
            if ($matches) {
                $found[] = $path;
            }
        }
    }
    // Stable ordering so re-runs show the same failure order.
    sort($found);
    return array_values(array_unique($found));
}

// ---------------------------------------------------------------------------
// Stage: load_tests (parse each test file, track which file defines which class)
// ---------------------------------------------------------------------------
pg_stage_begin('load_tests');
$suite = new PHPUnit\Framework\TestSuite('Playground Tests');
$before_classes = get_declared_classes();
try {
    foreach ($test_files as $tf) {
        require_once $tf;
    }
} catch (Throwable $e) {
    pg_stage_fail('load_tests', $e);
    exit(1);
}
$after_classes = get_declared_classes();
$new_classes = array_diff($after_classes, $before_classes);
foreach ($new_classes as $class_name) {
    try {
        $ref = new ReflectionClass($class_name);
        if (!$ref->isAbstract() && $ref->isSubclassOf('PHPUnit\\Framework\\TestCase')) {
            $suite->addTestSuite($class_name);
        }
    } catch (Throwable $e) {
        pg_log("NOTICE:reflection failed for $class_name: " . $e->getMessage());
    }
}
pg_stage_ok('load_tests');

/**
 * Parse the narrow PHPUnit CLI arguments Homeboy forwards into Playground.
 *
 * The host runner passes arguments after `/runner.php`, so `$argv` mirrors the
 * user-facing `homeboy test <component> -- <args>` contract. Keep this parser
 * intentionally small: unknown arguments are logged instead of silently changing
 * the run shape, while the common targeting flags reach PHPUnit's TestRunner.
 */
function pg_parse_phpunit_args(array $argv) {
    $arguments = [
        'colors' => 'never',
        'testdox' => true,
        'verbose' => false,
        // PHPUnit 9.6 reads $arguments['extensions'] unconditionally (line 1074
        // in TestRunner.php). Omit it and you get two warnings per run.
        'extensions' => [],
    ];

    $args = array_slice($argv, 1);
    for ($i = 0; $i < count($args); $i++) {
        $arg = $args[$i];
        if ($arg === '--filter') {
            if (isset($args[$i + 1])) {
                $arguments['filter'] = $args[++$i];
                pg_log('NOTICE:phpunit filter applied: ' . $arguments['filter']);
            } else {
                pg_log('NOTICE:phpunit --filter ignored because no value was provided');
            }
            continue;
        }
        if (strpos($arg, '--filter=') === 0) {
            $arguments['filter'] = substr($arg, strlen('--filter='));
            pg_log('NOTICE:phpunit filter applied: ' . $arguments['filter']);
            continue;
        }
        if ($arg === '--list-tests') {
            $arguments['listTests'] = true;
            pg_log('NOTICE:phpunit list-tests enabled');
            continue;
        }
        if ($arg === '--testdox') {
            $arguments['testdox'] = true;
            continue;
        }
        if ($arg === '--no-testdox') {
            $arguments['testdox'] = false;
            continue;
        }
        if ($arg === '--verbose' || $arg === '-v') {
            $arguments['verbose'] = true;
            continue;
        }
        if ($arg === '--colors=always') {
            $arguments['colors'] = 'always';
            continue;
        }
        if ($arg === '--colors=never') {
            $arguments['colors'] = 'never';
            continue;
        }
        pg_log('NOTICE:unsupported phpunit argument ignored by Playground runner: ' . $arg);
    }

    return $arguments;
}

/**
 * Print PHPUnit-style test names from a TestSuite tree for --list-tests.
 */
function pg_print_test_list($test) {
    if ($test instanceof PHPUnit\Framework\TestSuite) {
        foreach ($test->tests() as $child) {
            pg_print_test_list($child);
        }
        return;
    }

    if ($test instanceof PHPUnit\Framework\TestCase) {
        echo get_class($test) . '::' . $test->getName() . PHP_EOL;
    }
}

// ---------------------------------------------------------------------------
// Stage: run_tests
// ---------------------------------------------------------------------------
pg_stage_begin('run_tests');
pg_log("RUNNING " . count($test_files) . " TEST FILES");
try {
    $phpunit_args = pg_parse_phpunit_args($argv ?? []);
    if (!empty($phpunit_args['listTests'])) {
        pg_print_test_list($suite);
        pg_log('ALL TESTS PASSED');
        pg_log('TESTS: ' . $suite->count() . ' FAILURES: 0 ERRORS: 0');
        pg_stage_ok('run_tests');
        exit(0);
    }

    $runner = new PHPUnit\TextUI\TestRunner();
    $result = $runner->run($suite, $phpunit_args);
    pg_log($result->wasSuccessful() ? "ALL TESTS PASSED" : "SOME TESTS FAILED");
    pg_log("TESTS: " . $result->count() . " FAILURES: " . count($result->failures()) . " ERRORS: " . count($result->errors()));
    pg_stage_ok('run_tests');
    exit($result->wasSuccessful() ? 0 : 1);
} catch (Throwable $e) {
    pg_stage_fail('run_tests', $e);
    exit(1);
}
