<?php
error_reporting(E_ALL & ~E_WARNING & ~E_NOTICE & ~E_DEPRECATED);

$result_file = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}/.pg-test-result.txt';

function log_msg($msg) {
    global $result_file;
    file_put_contents($result_file, $msg . "\n", FILE_APPEND);
}

register_shutdown_function(function() {
    global $result_file;
    $error = error_get_last();
    if ($error && $error['type'] === E_ERROR) {
        file_put_contents($result_file, "FATAL: {$error['message']}\n", FILE_APPEND);
    }
});

$tests_dir = '/homeboy-extension/vendor/wp-phpunit/wp-phpunit';
$plugin_path = '/wordpress/wp-content/plugins/{{PLUGIN_SLUG}}';

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
log_msg("BOOT OK");

$argv = ['install.php', $config_path, 'no_ms_tests', 'no_core_tests'];
$_SERVER['argv'] = $argv;

log_msg("INSTALLING");
require_once "$tests_dir/includes/install.php";
log_msg("INSTALLED");

while (ob_get_level() > 0) @ob_end_clean();

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

log_msg("TEST_CLASSES_LOADED");

$dep_paths = '{{PLAYGROUND_DEP_MOUNTS}}';
if (!empty($dep_paths)) {
    foreach (explode("\n", $dep_paths) as $dep_mount) {
        $dep_mount = trim($dep_mount);
        if (empty($dep_mount)) continue;
        $dep_files = glob("$dep_mount/*.php");
        foreach ($dep_files as $df) {
            if (strpos(file_get_contents($df), 'Plugin Name:') !== false) {
                require_once $df;
                break;
            }
        }
    }
}

$style_css = "$plugin_path/style.css";
if (file_exists($style_css) && strpos(file_get_contents($style_css), 'Theme Name:') !== false) {
    log_msg("THEME_DETECTED");
    $fn_php = "$plugin_path/functions.php";
    if (file_exists($fn_php)) require_once $fn_php;
} else {
    $main_files = glob("$plugin_path/*.php");
    foreach ($main_files as $mf) {
        if (strpos(file_get_contents($mf), 'Plugin Name:') !== false) {
            log_msg("PLUGIN_DETECTED " . basename($mf));
            require_once $mf;
            break;
        }
    }
}

$test_dir = "$plugin_path/tests";
$test_files = array_merge(
    glob("$test_dir/test-*.php") ?: [],
    glob("$test_dir/*Test.php") ?: []
);
if (empty($test_files)) {
    log_msg("NO_TEST_FILES");
    exit(1);
}

$suite = new PHPUnit\Framework\TestSuite('Playground Tests');
$before_classes = get_declared_classes();
foreach ($test_files as $tf) {
    require_once $tf;
}
$after_classes = get_declared_classes();
$new_classes = array_diff($after_classes, $before_classes);
foreach ($new_classes as $class_name) {
    $ref = new ReflectionClass($class_name);
    if (!$ref->isAbstract() && $ref->isSubclassOf('PHPUnit\\Framework\\TestCase')) {
        $suite->addTestSuite($class_name);
    }
}

log_msg("RUNNING " . count($test_files) . " TEST FILES");
$runner = new PHPUnit\TextUI\TestRunner();
$result = $runner->run($suite, ['colors' => 'never', 'testdox' => true, 'verbose' => false]);
log_msg($result->wasSuccessful() ? "ALL TESTS PASSED" : "SOME TESTS FAILED");
log_msg("TESTS: " . $result->count() . " FAILURES: " . count($result->failures()) . " ERRORS: " . count($result->errors()));
exit($result->wasSuccessful() ? 0 : 1);
