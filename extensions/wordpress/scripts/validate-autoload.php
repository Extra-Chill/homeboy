<?php
/**
 * Pre-flight plugin load validation
 * Catches autoload errors, missing classes, and fatal errors during initialization
 */

$module_path = getenv('HOMEBOY_MODULE_PATH') ?: dirname(__DIR__);
$plugin_path = getenv('HOMEBOY_PLUGIN_PATH') ?: getenv('HOMEBOY_COMPONENT_PATH') ?: getcwd();

// WordPress paths from wp-phpunit
$wp_tests_dir = $module_path . '/vendor/wp-phpunit/wp-phpunit';
$abspath = $wp_tests_dir . '/wordpress/';

// Verify wp-phpunit exists
if (!is_dir($wp_tests_dir)) {
    echo "Error: WordPress test library not found at $wp_tests_dir\n";
    echo "Run 'composer install' in the WordPress module directory.\n";
    exit(1);
}

// Find plugin main file (same logic as bootstrap.php)
$plugin_file = find_plugin_main_file($plugin_path);
if (!$plugin_file) {
    echo "Could not find plugin main file in $plugin_path\n";
    echo "Looked for files with 'Plugin Name:' header\n";
    exit(1);
}

// Register shutdown handler for fatal errors
register_shutdown_function(function() use ($plugin_file) {
    $error = error_get_last();
    if ($error && in_array($error['type'], [E_ERROR, E_PARSE, E_CORE_ERROR, E_COMPILE_ERROR])) {
        echo "\n";
        echo "AUTOLOAD ERROR: " . $error['message'] . "\n\n";
        echo "  File: " . $error['file'] . "\n";
        echo "  Line: " . $error['line'] . "\n\n";
        echo "Possible causes:\n";
        echo "  - Missing composer autoload (run: composer dump-autoload)\n";
        echo "  - Class file doesn't exist at expected path\n";
        echo "  - Namespace mismatch between class and file location\n";
        echo "  - PSR-4 autoload config incorrect in composer.json\n";
        exit(1);
    }
});

// Set up minimal WordPress environment
define('ABSPATH', $abspath);
define('WPINC', 'wp-includes');

// Load WordPress test functions (provides tests_add_filter for test setup)
require_once $wp_tests_dir . '/includes/functions.php';

// Stub implementations for WordPress functions plugins call at load time
// These allow the plugin to load without fatal errors while we verify autoloading
if (!function_exists('add_action')) {
    function add_action($hook, $callback, $priority = 10, $accepted_args = 1) {
        return true;
    }
}
if (!function_exists('add_filter')) {
    function add_filter($hook, $callback, $priority = 10, $accepted_args = 1) {
        return true;
    }
}
if (!function_exists('register_activation_hook')) {
    function register_activation_hook($file, $callback) {
        return;
    }
}
if (!function_exists('register_deactivation_hook')) {
    function register_deactivation_hook($file, $callback) {
        return;
    }
}
if (!function_exists('plugin_dir_path')) {
    function plugin_dir_path($file) {
        return trailingslashit(dirname($file));
    }
}
if (!function_exists('plugin_dir_url')) {
    function plugin_dir_url($file) {
        return '';
    }
}
if (!function_exists('plugin_basename')) {
    function plugin_basename($file) {
        return basename(dirname($file)) . '/' . basename($file);
    }
}
if (!function_exists('trailingslashit')) {
    function trailingslashit($string) {
        return rtrim($string, '/\\') . '/';
    }
}
if (!function_exists('wp_die')) {
    function wp_die($message = '', $title = '', $args = array()) {
        exit(1);
    }
}

// Attempt to load plugin
require_once $plugin_file;

echo "Plugin loaded successfully.\n";
exit(0);

/**
 * Find plugin main file by scanning for Plugin Name header
 */
function find_plugin_main_file($path) {
    // Check common names first
    $candidates = [basename($path) . '.php', 'plugin.php'];
    foreach ($candidates as $name) {
        $file = $path . '/' . $name;
        if (file_exists($file) && strpos(file_get_contents($file), 'Plugin Name:') !== false) {
            return $file;
        }
    }
    // Scan root PHP files
    foreach (glob($path . '/*.php') as $file) {
        if (strpos(file_get_contents($file), 'Plugin Name:') !== false) {
            return $file;
        }
    }
    return null;
}
