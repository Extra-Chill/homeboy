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

// Detect component type and find appropriate file to load
$component_type = null;
$component_file = null;

// Check if this is a theme first
$style_css = $_plugin_path . '/style.css';
if (file_exists($style_css) && strpos(file_get_contents($style_css), 'Theme Name:') !== false) {
    $component_type = 'theme';
    $functions_php = $_plugin_path . '/functions.php';
    if (file_exists($functions_php)) {
        $component_file = $functions_php;
    }
} else {
    // Check if it's a plugin
    $files = glob($_plugin_path . '/*.php');
    foreach ($files as $file) {
        $content = file_get_contents($file);
        if (strpos($content, 'Plugin Name:') !== false) {
            $component_type = 'plugin';
            $component_file = $file;
            break;
        }
    }
}

if (!$component_type || !$component_file) {
    if (!$component_type) {
        echo "Could not detect component type in $_plugin_path\n";
        echo "Expected either a plugin (with 'Plugin Name:' header) or theme (with 'Theme Name:' in style.css)\n";
    } else {
        echo "Could not find main file for $component_type in $_plugin_path\n";
        if ($component_type === 'theme') {
            echo "Looked for functions.php in theme directory\n";
        } else {
            echo "Looked for files with 'Plugin Name:' header\n";
        }
    }
    exit(1);
}

if (!$_core_dir) {
    echo "ABSPATH not set\n";
    exit(1);
}

// Only print debug info when HOMEBOY_DEBUG is set
if (getenv('HOMEBOY_DEBUG') === '1') {
    echo "Detected $component_type with file: $component_file\n";
}

// Load component at the appropriate WordPress hook
if ($component_type === 'theme') {
    // Load themes on after_setup_theme hook
    tests_add_filter('after_setup_theme', function() use ($component_file, $_plugin_path) {
        if ($component_file) {
            require_once $component_file;
        }
        
        // Set theme constants for tests
        if (!defined('TEMPLATEPATH')) {
            define('TEMPLATEPATH', $_plugin_path);
        }
        if (!defined('STYLESHEETPATH')) {
            define('STYLESHEETPATH', $_plugin_path);
        }
    });
} else {
    // Load plugins on plugins_loaded hook
    tests_add_filter('plugins_loaded', function() use ($component_file) {
        require_once $component_file;
    });
}

// Start up the WP testing environment
require_once $_tests_dir . '/includes/bootstrap.php';
