<?php
/**
 * Plugin Name: Playground Init Lifecycle Host
 * Description: Fixture proving deferred init callbacks still run as init callbacks.
 */

add_action( 'init', 'homeboy_playground_init_lifecycle_callback', 20 );

function homeboy_playground_init_lifecycle_callback() {
    update_option(
        'homeboy_playground_init_lifecycle_context',
        doing_action( 'init' ) ? 'init' : 'not-init'
    );

    if ( class_exists( 'WP_Connector_Registry' ) ) {
        WP_Connector_Registry::set_instance( new WP_Connector_Registry() );
    }
}
