<?php
/**
 * Configured-workload follow-up step for the wp-cli plugin install smoke.
 *
 * Runs *after* the workload's wp-cli step installs and activates
 * `hello-dolly` from wordpress.org. Asserts the plugin is registered
 * as active and that its functions are loaded into the running PHP
 * process — proving the wp-cli step actually did the install + activate
 * inside the Playground PHP runtime.
 *
 * Returns metrics in the same shape configured workloads already use,
 * so the smoke can assert via jq against the BenchResults envelope.
 */

if ( ! function_exists( 'is_plugin_active' ) ) {
    require_once ABSPATH . 'wp-admin/includes/plugin.php';
}

$plugin_file = 'hello-dolly/hello.php';
$active      = is_plugin_active( $plugin_file ) ? 1 : 0;
$file_loaded = function_exists( 'hello_dolly_get_lyric' ) ? 1 : 0;

return [
    'metrics' => [
        'plugin_active'      => $active,
        'plugin_file_loaded' => $file_loaded,
    ],
    'metadata' => [
        'plugin_file'    => $plugin_file,
        'active_plugins' => array_values( (array) get_option( 'active_plugins', [] ) ),
    ],
];
