<?php

$report_path = __DIR__ . '/report.json';
$count_path = __DIR__ . '/count.txt';
$run_count = is_file($count_path) ? (int) file_get_contents($count_path) : 0;
$run_count++;
file_put_contents($count_path, (string) $run_count);

file_put_contents($report_path, json_encode([
    'ok' => true,
    'source' => 'configured-playground-workload',
    'run_count' => $run_count,
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
