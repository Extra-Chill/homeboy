<?php
/**
 * Plugin Name: Managed Dependency Order Fixture
 */

$GLOBALS['homeboy_managed_dependency_class_preloaded'] = class_exists(
    \HomeboyManagedDependencyOrder\Canary::class,
    false
);

require_once __DIR__ . '/vendor/autoload.php';

$GLOBALS['homeboy_managed_dependency_loaded'] = class_exists(
    \HomeboyManagedDependencyOrder\Canary::class
);

register_activation_hook( __FILE__, static function (): void {
    global $wpdb;

    if ( ! $wpdb instanceof wpdb ) {
        throw new RuntimeException( 'Managed dependency activation received an invalid $wpdb.' );
    }

    update_option( 'homeboy_managed_dependency_activated', \HomeboyManagedDependencyOrder\Canary::value() );
} );
