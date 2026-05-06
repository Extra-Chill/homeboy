<?php

return function (): array {
    $context = get_option( 'homeboy_playground_init_lifecycle_context' );
    if ( $context !== 'init' ) {
        throw new RuntimeException( 'Deferred init callback ran outside init context.' );
    }

    return [
        'metrics' => [
            'deferred_init_context_ok' => 1,
        ],
        'metadata' => [
            'fixture' => 'playground-init-lifecycle-host',
        ],
    ];
};
