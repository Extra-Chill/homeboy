<?php
/**
 * Plugin Name: Managed Dependency Order Host Fixture
 */

$GLOBALS['homeboy_managed_dependency_available_during_install'] = class_exists(
    \HomeboyManagedDependencyOrder\Canary::class,
    false
);

add_action( 'plugins_loaded', static function (): void {
    $GLOBALS['homeboy_managed_dependency_component_value'] = \HomeboyManagedDependencyOrder\Canary::value();
} );
