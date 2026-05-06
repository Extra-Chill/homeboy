<?php

$report_path = __DIR__ . '/report.json';
file_put_contents($report_path, json_encode([
    'ok' => true,
    'source' => 'configured-playground-workload',
], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));

return [
    'metrics' => [
        'generated_pages' => 2,
    ],
    'artifacts' => [
        'generated_report' => [
            'path' => 'workloads/report.json',
            'kind' => 'json',
            'label' => 'Generated workload report',
        ],
    ],
    'metadata' => [
        'phase' => 'configured',
        'preview_url' => 'https://example.test/playground-preview',
    ],
];
