<?php
return function (): array {
    $subject = get_option('scenario_manifest_subject');

    if ($subject !== 'navigation') {
        throw new RuntimeException('scenario manifest setup step did not run before verifier');
    }

    return [
        'metadata' => [
            'verifier' => 'passed',
        ],
    ];
};
