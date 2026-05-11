<?php
return function (): array {
    $subject = get_option('scenario_manifest_subject');

    if ($subject !== 'navigation') {
        throw new RuntimeException('scenario manifest setup step did not run before grader');
    }

    return [
        'metrics' => [
            'grade' => 1,
        ],
        'metadata' => [
            'grader' => 'passed',
        ],
    ];
};
