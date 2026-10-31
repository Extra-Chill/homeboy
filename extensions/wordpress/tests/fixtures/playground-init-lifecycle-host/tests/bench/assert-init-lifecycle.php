<?php

return function (): array {
    $context = get_option( 'homeboy_playground_init_lifecycle_context' );
    if ( $context !== 'init' ) {
        throw new RuntimeException( 'Deferred init callback ran outside init context.' );
    }

    $activation_seen = get_option( 'homeboy_playground_init_lifecycle_activation_seen' );
    if ( $activation_seen !== 'yes' ) {
        throw new RuntimeException( 'Deferred init callback ran before plugin activation.' );
    }

    $plugins_loaded_context = get_option( 'homeboy_playground_plugins_loaded_lifecycle_context' );
    if ( $plugins_loaded_context !== 'plugins_loaded' ) {
        throw new RuntimeException( 'Deferred plugins_loaded callback ran outside plugins_loaded context.' );
    }

    $plugins_loaded_activation_seen = get_option( 'homeboy_playground_plugins_loaded_lifecycle_activation_seen' );
    if ( $plugins_loaded_activation_seen !== 'yes' ) {
        throw new RuntimeException( 'Deferred plugins_loaded callback ran before plugin activation.' );
    }

    return [
        'metrics' => [
            'deferred_init_context_ok' => 1,
            'deferred_init_after_activation_ok' => 1,
            'deferred_plugins_loaded_context_ok' => 1,
            'deferred_plugins_loaded_after_activation_ok' => 1,
        ],
        'metadata' => [
            'fixture' => 'playground-init-lifecycle-host',
        ],
    ];
};
