<?php
/**
 * Homeboy WordPress Module Bootstrap
 *
 * Provides complete WordPress testing infrastructure.
 * Components only need test files - no WordPress setup required.
 */

$_tests_dir = getenv('WP_TESTS_DIR');
$_core_dir = getenv('ABSPATH');

// Determine plugin path (component or project)
if (getenv('HOMEBOY_COMPONENT_PATH')) {
    // Component-level testing
    $_plugin_path = getenv('HOMEBOY_COMPONENT_PATH');
} elseif (getenv('HOMEBOY_PROJECT_PATH')) {
    // Project-level testing
    $_plugin_path = getenv('HOMEBOY_PROJECT_PATH');
} elseif (getenv('HOMEBOY_PLUGIN_PATH')) {
    // Explicit plugin path
    $_plugin_path = getenv('HOMEBOY_PLUGIN_PATH');
} else {
    // Fallback - assume current directory
    $_plugin_path = getcwd();
}

// Set required WordPress test constants
if (!defined('WP_TESTS_DOMAIN')) {
    define('WP_TESTS_DOMAIN', 'example.org');
}
if (!defined('WP_TESTS_EMAIL')) {
    define('WP_TESTS_EMAIL', 'admin@example.org');
}
if (!defined('WP_TESTS_TITLE')) {
    define('WP_TESTS_TITLE', 'Test Blog');
}
if (!defined('WP_PHP_BINARY')) {
    define('WP_PHP_BINARY', 'php');
}
if (!defined('WP_TESTS_NETWORK_TITLE')) {
    define('WP_TESTS_NETWORK_TITLE', 'Test Network');
}

// Define plugin constants for tests
define('TESTS_PLUGIN_DIR', $_plugin_path);

// Define WP_CORE_DIR
if (!defined('WP_CORE_DIR')) {
    define('WP_CORE_DIR', $_core_dir);
}

// Handle PHPUnit Polyfills (required for WordPress test suite)
$_phpunit_polyfills_path = getenv('WP_TESTS_PHPUNIT_POLYFILLS_PATH');
if (false !== $_phpunit_polyfills_path) {
    define('WP_TESTS_PHPUNIT_POLYFILLS_PATH', $_phpunit_polyfills_path);
} elseif (file_exists(__DIR__ . '/../vendor/yoast/phpunit-polyfills/phpunitpolyfills-autoload.php')) {
    // Use polyfills from WordPress module
    define('WP_TESTS_PHPUNIT_POLYFILLS_PATH', __DIR__ . '/../vendor/yoast/phpunit-polyfills');
} elseif (file_exists($_plugin_path . '/vendor/yoast/phpunit-polyfills/phpunitpolyfills-autoload.php')) {
    // Fallback to component's polyfills
    define('WP_TESTS_PHPUNIT_POLYFILLS_PATH', $_plugin_path . '/vendor/yoast/phpunit-polyfills');
}

// Load WordPress test functions
require_once "{$_tests_dir}/includes/functions.php";

// Find plugin main file
$plugin_file = null;

// First, try common plugin filenames
$common_names = ['my-plugin.php', basename($_plugin_path) . '.php'];
foreach ($common_names as $name) {
    $candidate = $_plugin_path . '/' . $name;
    if (file_exists($candidate)) {
        $content = file_get_contents($candidate);
        if (strpos($content, 'Plugin Name:') !== false) {
            $plugin_file = $candidate;
            break;
        }
    }
}

// If not found, scan all PHP files for plugin header
if (!$plugin_file) {
    $files = glob($_plugin_path . '/*.php');
    foreach ($files as $file) {
        $content = file_get_contents($file);
        if (strpos($content, 'Plugin Name:') !== false) {
            $plugin_file = $file;
            break;
        }
    }
}

if (!$plugin_file) {
    echo "Could not find plugin main file in $_plugin_path\n";
    echo "Looked for files with 'Plugin Name:' header\n";
    exit(1);
}

if (!$_core_dir) {
    echo "ABSPATH not set\n";
    exit(1);
}

// Find plugin main file
$plugin_file = null;

// First, try common plugin filenames
$common_names = [basename($_plugin_path) . '.php', 'my-plugin.php'];
foreach ($common_names as $name) {
    $candidate = $_plugin_path . '/' . $name;
    if (file_exists($candidate)) {
        $content = file_get_contents($candidate);
        if (strpos($content, 'Plugin Name:') !== false) {
            $plugin_file = $candidate;
            break;
        }
    }
}

// If not found, scan all PHP files for plugin header
if (!$plugin_file) {
    $files = glob($_plugin_path . '/*.php');
    foreach ($files as $file) {
        $content = file_get_contents($file);
        if (strpos($content, 'Plugin Name:') !== false) {
            $plugin_file = $file;
            break;
        }
    }
}

if (!$plugin_file) {
    echo "Could not find plugin main file in $_plugin_path\n";
    echo "Looked for files with 'Plugin Name:' header\n";
    exit(1);
}

// Only print debug info when HOMEBOY_DEBUG is set
if (getenv('HOMEBOY_DEBUG') === '1') {
    echo "Found plugin file: $plugin_file\n";
}

// Load plugin before WordPress loads
tests_add_filter('muplugins_loaded', function() use ($plugin_file) {
    require_once $plugin_file;
});

// Start up the WP testing environment
require_once $_tests_dir . '/includes/bootstrap.php';
