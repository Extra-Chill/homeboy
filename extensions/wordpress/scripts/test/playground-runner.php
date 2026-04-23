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
 *   THEME_DETECTED               - loaded theme (style.css + functions.php)
 *   NO_TEST_FILES                - discovery found no candidates
 *   RUNNING <n> TEST FILES       - starting PHPUnit
 *   ALL TESTS PASSED             - result.wasSuccessful() == true
 *   SOME TESTS FAILED            - result.wasSuccessful() == false
 *   TESTS: <n> FAILURES: <n> ERRORS: <n>
 *
 * Stages (in execution order):
 *   boot             - composer autoload + config write
 *   install          - wp-phpunit install.php (creates WP + tables)
 *   load_fixtures    - test case classes, mock mailer, harness filters
 *   load_deps        - dependency plugin bootstrap files (if any)
 *   load_component   - plugin/theme under test
 *   discover_tests   - glob test files
 *   load_tests       - require_once each test file
 *   run_tests        - PHPUnit execution
 *
 * The bash runner's job: if playground_exit != 0 AND no STAGE_FAIL/FATAL/
 * SOME TESTS FAILED line exists, surface the raw stdout/stderr — something
 * crashed before we could even write to the result file.
 */

// Report everything. Warnings/notices inside WP bootstrap have historically
// been the silent failure mode — the old ~E_WARNING mask hid real problems.
error_reporting(E_ALL);
ini_set('display_errors', '1');
ini_set('display_startup_errors', '1');

$result_file = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}/.pg-test-result.txt';
$current_stage = 'preboot';

function pg_log($msg) {
    global $result_file;
    file_put_contents($result_file, $msg . "\n", FILE_APPEND);
}

function pg_stage_begin($stage) {
    global $current_stage;
    $current_stage = $stage;
    pg_log("STAGE_BEGIN:$stage");
}

function pg_stage_ok($stage) {
    pg_log("STAGE_OK:$stage");
}

function pg_stage_fail($stage, Throwable $e) {
    $msg = get_class($e) . ': ' . $e->getMessage()
        . ' at ' . $e->getFile() . ':' . $e->getLine();
    pg_log("STAGE_FAIL:$stage:$msg");
    // Also dump trace for debugging; bash runner will print it under failure.
    pg_log("TRACE:");
    foreach (explode("\n", $e->getTraceAsString()) as $line) {
        pg_log("  $line");
    }
}

// Capture non-fatal warnings/notices so they don't vanish into the void.
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
    pg_log("NOTICE:$label: $message at $file:$line");
    return false; // let PHP's default handler also run
});

register_shutdown_function(function () {
    global $current_stage;
    $error = error_get_last();
    if ($error && in_array($error['type'], [E_ERROR, E_PARSE, E_CORE_ERROR, E_COMPILE_ERROR], true)) {
        pg_log("STAGE_FATAL:$current_stage:{$error['message']} at {$error['file']}:{$error['line']}");
    }
});

$tests_dir = '/homeboy-extension/vendor/wp-phpunit/wp-phpunit';
$plugin_path = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}';

// ---------------------------------------------------------------------------
// Stage: boot
// ---------------------------------------------------------------------------
pg_stage_begin('boot');
try {
    $config_path = '/tmp/wp-tests-config.php';
    file_put_contents($config_path, <<<'CONFIG'
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
CONFIG
    );

    require_once '/homeboy-extension/vendor/autoload.php';
    pg_stage_ok('boot');
} catch (Throwable $e) {
    pg_stage_fail('boot', $e);
    exit(1);
}

// ---------------------------------------------------------------------------
// Stage: install (wp-phpunit install.php creates WP tables in-process)
// ---------------------------------------------------------------------------
pg_stage_begin('install');
try {
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

// ---------------------------------------------------------------------------
// Stage: load_fixtures (test case classes, mock mailer, harness filters)
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
// Stage: load_deps (dependency plugin bootstrap files)
// ---------------------------------------------------------------------------
pg_stage_begin('load_deps');
try {
    $dep_paths = '{{PLAYGROUND_DEP_MOUNTS}}';
    if (!empty($dep_paths)) {
        foreach (explode("\n", $dep_paths) as $dep_mount) {
            $dep_mount = trim($dep_mount);
            if (empty($dep_mount)) {
                continue;
            }
            $dep_files = glob("$dep_mount/*.php") ?: [];
            foreach ($dep_files as $df) {
                if (strpos(file_get_contents($df), 'Plugin Name:') !== false) {
                    require_once $df;
                    break;
                }
            }
        }
    }
    pg_stage_ok('load_deps');
} catch (Throwable $e) {
    pg_stage_fail('load_deps', $e);
    exit(1);
}

// ---------------------------------------------------------------------------
// Stage: load_component (plugin or theme under test)
// ---------------------------------------------------------------------------
pg_stage_begin('load_component');
try {
    $style_css = "$plugin_path/style.css";
    if (file_exists($style_css) && strpos(file_get_contents($style_css), 'Theme Name:') !== false) {
        pg_log("THEME_DETECTED");
        $fn_php = "$plugin_path/functions.php";
        if (file_exists($fn_php)) {
            require_once $fn_php;
        }
    } else {
        $loaded = false;
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
                require_once $mf;
                $loaded = true;
                break;
            }
        }
        if (!$loaded) {
            pg_log("NOTICE:no plugin entry file with 'Plugin Name:' header found in $plugin_path");
        }
    }
    pg_stage_ok('load_component');
} catch (Throwable $e) {
    pg_stage_fail('load_component', $e);
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
 * any relative `<directory>` entry in the XML is resolved against
 * $plugin_path/tests (the component under test, NOT the extension's own
 * vendor dir — the XML's path semantics are "relative to the plugin being
 * tested").
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

// ---------------------------------------------------------------------------
// Stage: run_tests
// ---------------------------------------------------------------------------
pg_stage_begin('run_tests');
pg_log("RUNNING " . count($test_files) . " TEST FILES");
try {
    $runner = new PHPUnit\TextUI\TestRunner();
    $result = $runner->run($suite, [
        'colors' => 'never',
        'testdox' => true,
        'verbose' => false,
        // PHPUnit 9.6 reads $arguments['extensions'] unconditionally (line 1074
        // in TestRunner.php). Omit it and you get two warnings per run.
        'extensions' => [],
    ]);
    pg_log($result->wasSuccessful() ? "ALL TESTS PASSED" : "SOME TESTS FAILED");
    pg_log("TESTS: " . $result->count() . " FAILURES: " . count($result->failures()) . " ERRORS: " . count($result->errors()));
    pg_stage_ok('run_tests');
    exit($result->wasSuccessful() ? 0 : 1);
} catch (Throwable $e) {
    pg_stage_fail('run_tests', $e);
    exit(1);
}
