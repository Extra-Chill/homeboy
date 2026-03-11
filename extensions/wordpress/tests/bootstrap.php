<?php
/**
 * Homeboy WordPress Extension Bootstrap
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

$_dependency_paths = array_values(
	array_filter(
		array_map(
			'trim',
			explode("\n", (string) getenv('HOMEBOY_WORDPRESS_DEPENDENCY_PATHS'))
		)
	)
);

// Define WP_CORE_DIR
if (!defined('WP_CORE_DIR')) {
    define('WP_CORE_DIR', $_core_dir);
}

// Handle PHPUnit Polyfills (required for WordPress test suite)
$_phpunit_polyfills_path = getenv('WP_TESTS_PHPUNIT_POLYFILLS_PATH');
if (false !== $_phpunit_polyfills_path) {
    define('WP_TESTS_PHPUNIT_POLYFILLS_PATH', $_phpunit_polyfills_path);
} elseif (file_exists(__DIR__ . '/../vendor/yoast/phpunit-polyfills/phpunitpolyfills-autoload.php')) {
    // Use polyfills from WordPress extension
    define('WP_TESTS_PHPUNIT_POLYFILLS_PATH', __DIR__ . '/../vendor/yoast/phpunit-polyfills');
} elseif (file_exists($_plugin_path . '/vendor/yoast/phpunit-polyfills/phpunitpolyfills-autoload.php')) {
    // Fallback to component's polyfills
    define('WP_TESTS_PHPUNIT_POLYFILLS_PATH', $_plugin_path . '/vendor/yoast/phpunit-polyfills');
}

// Load WordPress test functions
require_once "{$_tests_dir}/includes/functions.php";

function homeboy_find_component_main_file( string $path ): ?array {
	$style_css = $path . '/style.css';
	if ( file_exists( $style_css ) && false !== strpos( file_get_contents( $style_css ), 'Theme Name:' ) ) {
		$functions_php = $path . '/functions.php';
		if ( file_exists( $functions_php ) ) {
			return array(
				'type' => 'theme',
				'file' => $functions_php,
			);
		}

		return array(
			'type' => 'theme',
			'file' => null,
		);
	}

	$files = glob( $path . '/*.php' );
	if ( false === $files ) {
		return null;
	}

	foreach ( $files as $file ) {
		$content = file_get_contents( $file );
		if ( false !== $content && false !== strpos( $content, 'Plugin Name:' ) ) {
			return array(
				'type' => 'plugin',
				'file' => $file,
			);
		}
	}

	return null;
}

function homeboy_load_dependency_components( array $dependency_paths ): void {
	foreach ( $dependency_paths as $dependency_path ) {
		$component = homeboy_find_component_main_file( $dependency_path );
		if ( ! $component || 'plugin' !== $component['type'] || empty( $component['file'] ) ) {
			continue;
		}

		require_once $component['file'];
	}
}

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
    $component = homeboy_find_component_main_file($_plugin_path);
    if ($component && 'plugin' === $component['type']) {
        $component_type = 'plugin';
        $component_file = $component['file'];
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
    tests_add_filter('plugins_loaded', function() use ($_dependency_paths) {
        homeboy_load_dependency_components($_dependency_paths);
    });

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
    tests_add_filter('plugins_loaded', function() use ($component_file, $_dependency_paths) {
        homeboy_load_dependency_components($_dependency_paths);
        require_once $component_file;
    });
}

// Prevent wp_not_installed() from killing the process.
//
// When wp-settings.php loads, it calls wp_not_installed() which checks
// is_blog_installed(). With the SQLite database driver, this check can
// return false even after install.php has run (the lightweight driver
// doesn't fully satisfy all the queries is_blog_installed() makes).
// When is_blog_installed() returns false, wp_not_installed() calls
// wp_redirect() + die() — silently terminating the PHPUnit process
// before any tests run, producing zero output with exit code 0.
//
// Defining WP_INSTALLING makes wp_installing() return true, which
// causes wp_not_installed() to bail out early (line 943 of load.php:
// "if ( is_blog_installed() || wp_installing() ) { return; }").
if (!defined('WP_INSTALLING')) {
    define('WP_INSTALLING', true);
}

// Start up the WP testing environment
require_once $_tests_dir . '/includes/bootstrap.php';

// Turn off installing mode so tests run against a normally-loaded WordPress.
// wp_installing() uses a static variable, so this call overrides the constant.
wp_installing(false);

// Clean WordPress output buffers so PHPUnit's result printer works.
// The WP bootstrap starts ob_start() during initialization which captures
// all subsequent stdout, including PHPUnit's test output.
// We use ob_end_clean() (not ob_end_flush) to discard the buffered bootstrap
// noise ("Installing...", "Running as single site...") silently — flushing it
// triggers "Cannot modify header information" warnings that corrupt output.
while ( ob_get_level() > 0 ) {
	ob_end_clean();
}
