<?php
/** Workload that emits custom metrics + metadata for the bench runner. */
return function (): array {
    return [
        'metrics' => [
            'rows' => 10,
            'changed_files' => 3,
            'ignored_label' => 'not numeric',
        ],
        'metadata' => [
            'phase' => 'warm',
        ],
    ];
};
