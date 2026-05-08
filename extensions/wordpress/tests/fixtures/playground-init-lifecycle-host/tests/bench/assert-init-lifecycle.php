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

    return [
        'metrics' => [
            'deferred_init_context_ok' => 1,
            'deferred_init_after_activation_ok' => 1,
        ],
        'metadata' => [
            'fixture' => 'playground-init-lifecycle-host',
        ],
    ];
};
