<?php
/**
 * Plugin Name: Playground Init Lifecycle Host
 * Description: Fixture proving deferred init callbacks still run as init callbacks.
 */

add_action( 'init', 'homeboy_playground_init_lifecycle_callback', 20 );
add_action( 'plugins_loaded', 'homeboy_playground_plugins_loaded_lifecycle_callback', 20 );
register_activation_hook( __FILE__, 'homeboy_playground_init_lifecycle_activate' );

function homeboy_playground_init_lifecycle_activate() {
    update_option( 'homeboy_playground_init_lifecycle_activated', 'yes' );
}

function homeboy_playground_plugins_loaded_lifecycle_callback() {
    update_option(
        'homeboy_playground_plugins_loaded_lifecycle_context',
        doing_action( 'plugins_loaded' ) ? 'plugins_loaded' : 'not-plugins-loaded'
    );

    update_option(
        'homeboy_playground_plugins_loaded_lifecycle_activation_seen',
        get_option( 'homeboy_playground_init_lifecycle_activated' ) === 'yes' ? 'yes' : 'no'
    );
}

function homeboy_playground_init_lifecycle_callback() {
    update_option(
        'homeboy_playground_init_lifecycle_context',
        doing_action( 'init' ) ? 'init' : 'not-init'
    );

    update_option(
        'homeboy_playground_init_lifecycle_activation_seen',
        get_option( 'homeboy_playground_init_lifecycle_activated' ) === 'yes' ? 'yes' : 'no'
    );

    if ( class_exists( 'WP_Connector_Registry' ) ) {
        WP_Connector_Registry::set_instance( new WP_Connector_Registry() );
    }
}
