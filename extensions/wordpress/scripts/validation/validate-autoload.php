<?php
/**
 * Pre-flight component load validation (plugins and themes)
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

// Find component main file (plugin or theme)
$component = find_component_main_file($plugin_path);
if (!$component) {
    echo "Could not find component main file in $plugin_path\n";
    echo "Looked for: style.css with 'Theme Name:' or *.php with 'Plugin Name:'\n";
    exit(1);
}

// Register shutdown handler for fatal errors
register_shutdown_function(function() use ($component) {
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
if (!function_exists('is_admin')) {
    function is_admin() {
        return false;
    }
}

// Options API stubs
if (!function_exists('get_option')) {
    function get_option($option, $default = false) {
        return $default;
    }
}
if (!function_exists('get_site_option')) {
    function get_site_option($option, $default = false) {
        return $default;
    }
}
if (!function_exists('update_option')) {
    function update_option($option, $value, $autoload = null) {
        return true;
    }
}
if (!function_exists('update_site_option')) {
    function update_site_option($option, $value) {
        return true;
    }
}
if (!function_exists('delete_option')) {
    function delete_option($option) {
        return true;
    }
}
if (!function_exists('delete_site_option')) {
    function delete_site_option($option) {
        return true;
    }
}
if (!function_exists('add_option')) {
    function add_option($option, $value = '', $deprecated = '', $autoload = 'yes') {
        return true;
    }
}
if (!function_exists('add_site_option')) {
    function add_site_option($option, $value) {
        return true;
    }
}

// Multisite stubs
if (!function_exists('is_multisite')) {
    function is_multisite() {
        return false;
    }
}
if (!function_exists('get_current_blog_id')) {
    function get_current_blog_id() {
        return 1;
    }
}
if (!function_exists('get_current_network_id')) {
    function get_current_network_id() {
        return 1;
    }
}
if (!function_exists('get_network_option')) {
    function get_network_option($network_id, $option, $default = false) {
        return $default;
    }
}
if (!function_exists('switch_to_blog')) {
    function switch_to_blog($blog_id, $deprecated = null) {
        return true;
    }
}
if (!function_exists('restore_current_blog')) {
    function restore_current_blog() {
        return true;
    }
}

// Caching stubs
if (!function_exists('wp_cache_get')) {
    function wp_cache_get($key, $group = '', $force = false, &$found = null) {
        $found = false;
        return false;
    }
}
if (!function_exists('wp_cache_set')) {
    function wp_cache_set($key, $data, $group = '', $expire = 0) {
        return true;
    }
}
if (!function_exists('wp_cache_add')) {
    function wp_cache_add($key, $data, $group = '', $expire = 0) {
        return true;
    }
}
if (!function_exists('wp_cache_delete')) {
    function wp_cache_delete($key, $group = '') {
        return true;
    }
}

// Hook state check stubs
if (!function_exists('did_action')) {
    function did_action($hook_name) {
        return 0;
    }
}
if (!function_exists('doing_action')) {
    function doing_action($hook_name = null) {
        return false;
    }
}
if (!function_exists('doing_filter')) {
    function doing_filter($hook_name = null) {
        return false;
    }
}
if (!function_exists('current_filter')) {
    function current_filter() {
        return '';
    }
}
if (!function_exists('has_action')) {
    function has_action($hook_name, $callback = false) {
        return false;
    }
}
if (!function_exists('has_filter')) {
    function has_filter($hook_name, $callback = false) {
        return false;
    }
}
if (!function_exists('remove_action')) {
    function remove_action($hook_name, $callback, $priority = 10) {
        return true;
    }
}
if (!function_exists('remove_filter')) {
    function remove_filter($hook_name, $callback, $priority = 10) {
        return true;
    }
}
if (!function_exists('do_action')) {
    function do_action($hook_name, ...$args) {
        return;
    }
}
if (!function_exists('do_action_ref_array')) {
    function do_action_ref_array($hook_name, $args) {
        return;
    }
}
if (!function_exists('apply_filters')) {
    function apply_filters($hook_name, $value, ...$args) {
        return $value;
    }
}
if (!function_exists('apply_filters_ref_array')) {
    function apply_filters_ref_array($hook_name, $args) {
        return $args[0] ?? null;
    }
}

// User/Auth stubs
if (!function_exists('get_current_user_id')) {
    function get_current_user_id() {
        return 0;
    }
}
if (!function_exists('is_user_logged_in')) {
    function is_user_logged_in() {
        return false;
    }
}
if (!function_exists('current_user_can')) {
    function current_user_can($capability, ...$args) {
        return false;
    }
}
if (!function_exists('wp_get_current_user')) {
    function wp_get_current_user() {
        return (object) ['ID' => 0, 'user_login' => '', 'user_email' => ''];
    }
}

// i18n stubs
if (!function_exists('__')) {
    function __($text, $domain = 'default') {
        return $text;
    }
}
if (!function_exists('_e')) {
    function _e($text, $domain = 'default') {
        echo $text;
    }
}
if (!function_exists('esc_html__')) {
    function esc_html__($text, $domain = 'default') {
        return $text;
    }
}
if (!function_exists('esc_attr__')) {
    function esc_attr__($text, $domain = 'default') {
        return $text;
    }
}
if (!function_exists('_n')) {
    function _n($single, $plural, $number, $domain = 'default') {
        return ($number == 1) ? $single : $plural;
    }
}
if (!function_exists('_x')) {
    function _x($text, $context, $domain = 'default') {
        return $text;
    }
}

// Escaping stubs
if (!function_exists('esc_html')) {
    function esc_html($text) {
        return $text;
    }
}
if (!function_exists('esc_attr')) {
    function esc_attr($text) {
        return $text;
    }
}
if (!function_exists('esc_url')) {
    function esc_url($url, $protocols = null, $_context = 'display') {
        return $url;
    }
}
if (!function_exists('esc_sql')) {
    function esc_sql($data) {
        return $data;
    }
}

// Sanitization stubs
if (!function_exists('wp_kses_post')) {
    function wp_kses_post($data) {
        return $data;
    }
}
if (!function_exists('sanitize_text_field')) {
    function sanitize_text_field($str) {
        return $str;
    }
}

// Shortcode API stubs
if (!function_exists('add_shortcode')) {
    function add_shortcode($tag, $callback) {
        return true;
    }
}
if (!function_exists('remove_shortcode')) {
    function remove_shortcode($tag) {
        return true;
    }
}
if (!function_exists('shortcode_exists')) {
    function shortcode_exists($tag) {
        return false;
    }
}
if (!function_exists('do_shortcode')) {
    function do_shortcode($content) {
        return $content;
    }
}
if (!function_exists('shortcode_atts')) {
    function shortcode_atts($pairs, $atts, $shortcode = '') {
        $atts = (array)$atts;
        $out = [];
        foreach ($pairs as $name => $default) {
            $out[$name] = array_key_exists($name, $atts) ? $atts[$name] : $default;
        }
        return $out;
    }
}

// Utility stubs
if (!function_exists('absint')) {
    function absint($maybeint) {
        return abs((int) $maybeint);
    }
}
if (!function_exists('wp_parse_args')) {
    function wp_parse_args($args, $defaults = array()) {
        if (is_object($args)) {
            $parsed_args = get_object_vars($args);
        } elseif (is_array($args)) {
            $parsed_args = $args;
        } else {
            parse_str($args, $parsed_args);
        }
        return array_merge($defaults, $parsed_args);
    }
}
if (!function_exists('wp_json_encode')) {
    function wp_json_encode($data, $options = 0, $depth = 512) {
        return json_encode($data, $options, $depth);
    }
}
if (!function_exists('wp_unslash')) {
    function wp_unslash($value) {
        if (is_array($value)) {
            return array_map('wp_unslash', $value);
        }
        return stripslashes($value);
    }
}

// Define common constants if not defined
if (!defined('DOING_AJAX')) {
    define('DOING_AJAX', false);
}
if (!defined('DOING_CRON')) {
    define('DOING_CRON', false);
}
if (!defined('WP_DEBUG')) {
    define('WP_DEBUG', false);
}

// Handle based on component type
if ($component['type'] === 'theme') {
    // Add theme-specific stubs
    if (!function_exists('get_template_directory')) {
        function get_template_directory() {
            return getenv('HOMEBOY_PLUGIN_PATH');
        }
    }
    if (!function_exists('get_stylesheet_directory')) {
        function get_stylesheet_directory() {
            return getenv('HOMEBOY_PLUGIN_PATH');
        }
    }
    if (!function_exists('get_template_directory_uri')) {
        function get_template_directory_uri() {
            return '';
        }
    }
    if (!function_exists('get_stylesheet_directory_uri')) {
        function get_stylesheet_directory_uri() {
            return '';
        }
    }
    if (!function_exists('get_stylesheet_uri')) {
        function get_stylesheet_uri() {
            return '';
        }
    }
    if (!function_exists('add_theme_support')) {
        function add_theme_support($feature, $args = array()) {
            return;
        }
    }

    if ($component['file']) {
        require_once $component['file'];
        echo "Theme loaded successfully.\n";
    } else {
        echo "Theme has no functions.php (CSS-only theme).\n";
    }
} else {
    // Plugin loading
    require_once $component['file'];
    echo "Plugin loaded successfully.\n";
}

exit(0);

/**
 * Find component main file - plugin or theme
 * Returns array: ['type' => 'plugin'|'theme', 'file' => '/path/to/file'] or null
 */
function find_component_main_file($path) {
    // Check for theme first (style.css with Theme Name:)
    $style_css = $path . '/style.css';
    if (file_exists($style_css) && strpos(file_get_contents($style_css), 'Theme Name:') !== false) {
        // For themes, return functions.php as the main file to load
        $functions_php = $path . '/functions.php';
        if (file_exists($functions_php)) {
            return ['type' => 'theme', 'file' => $functions_php];
        }
        // Theme without functions.php is valid (just CSS)
        return ['type' => 'theme', 'file' => null];
    }

    // Check for plugin (check common names first)
    $candidates = [basename($path) . '.php', 'plugin.php'];
    foreach ($candidates as $name) {
        $file = $path . '/' . $name;
        if (file_exists($file) && strpos(file_get_contents($file), 'Plugin Name:') !== false) {
            return ['type' => 'plugin', 'file' => $file];
        }
    }
    // Scan root PHP files
    foreach (glob($path . '/*.php') as $file) {
        if (strpos(file_get_contents($file), 'Plugin Name:') !== false) {
            return ['type' => 'plugin', 'file' => $file];
        }
    }
    return null;
}
