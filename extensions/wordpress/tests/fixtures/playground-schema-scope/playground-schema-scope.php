<?php
/**
 * Plugin Name: Playground Schema Scope Fixture
 * Description: Fixture plugin for Playground activation and changed-test scope smokes.
 * Version: 1.0.0
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

register_activation_hook( __FILE__, 'homeboy_playground_schema_scope_activate' );

function homeboy_playground_schema_scope_activate() {
    global $wpdb;

    $table = $wpdb->prefix . 'playground_schema_scope';
    $wpdb->query( "CREATE TABLE $table (id INTEGER PRIMARY KEY, label TEXT NOT NULL)" );
    update_option( 'homeboy_playground_schema_scope_activated', 'yes' );
}
