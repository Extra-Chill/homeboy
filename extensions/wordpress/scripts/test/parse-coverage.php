<?php
/**
 * Parse a Clover XML coverage report and output JSON summary.
 *
 * Usage: php parse-coverage.php <clover.xml> [source_dir]
 *
 * Output: JSON object with total coverage and per-file breakdown.
 * Writes to stdout. Exit code 0 on success, 1 on error.
 */

if ($argc < 2) {
    fwrite(STDERR, "Usage: php parse-coverage.php <clover.xml> [source_dir]\n");
    exit(1);
}

$clover_file = $argv[1];
$source_dir  = isset($argv[2]) ? rtrim($argv[2], '/') . '/' : '';

if (!file_exists($clover_file)) {
    fwrite(STDERR, "Error: Clover file not found: {$clover_file}\n");
    exit(1);
}

$xml = @simplexml_load_file($clover_file);
if ($xml === false) {
    fwrite(STDERR, "Error: Failed to parse Clover XML\n");
    exit(1);
}

$total_statements   = 0;
$covered_statements = 0;
$total_methods      = 0;
$covered_methods    = 0;
$total_classes       = 0;
$covered_classes     = 0;
$files               = [];

foreach ($xml->project->package as $package) {
    foreach ($package->file as $file) {
        process_file($file, $source_dir, $files, $total_statements, $covered_statements, $total_methods, $covered_methods, $total_classes, $covered_classes);
    }
}

// Also handle files not in packages
foreach ($xml->project->file as $file) {
    process_file($file, $source_dir, $files, $total_statements, $covered_statements, $total_methods, $covered_methods, $total_classes, $covered_classes);
}

$line_pct   = $total_statements > 0 ? round(($covered_statements / $total_statements) * 100, 2) : 0;
$method_pct = $total_methods > 0 ? round(($covered_methods / $total_methods) * 100, 2) : 0;
$class_pct  = $total_classes > 0 ? round(($covered_classes / $total_classes) * 100, 2) : 0;

// Sort files by coverage ascending (worst first)
usort($files, function ($a, $b) {
    return $a['line_pct'] <=> $b['line_pct'];
});

$result = [
    'totals' => [
        'lines'   => [
            'total'   => $total_statements,
            'covered' => $covered_statements,
            'pct'     => $line_pct,
        ],
        'methods' => [
            'total'   => $total_methods,
            'covered' => $covered_methods,
            'pct'     => $method_pct,
        ],
        'classes' => [
            'total'   => $total_classes,
            'covered' => $covered_classes,
            'pct'     => $class_pct,
        ],
    ],
    'files'  => $files,
];

echo json_encode($result, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . "\n";

function process_file($file, $source_dir, &$files, &$total_statements, &$covered_statements, &$total_methods, &$covered_methods, &$total_classes, &$covered_classes) {
    $metrics = $file->metrics;
    if (!$metrics) {
        return;
    }

    $file_path = (string) $file['name'];

    // Strip source_dir prefix for relative paths
    if ($source_dir && strpos($file_path, $source_dir) === 0) {
        $file_path = substr($file_path, strlen($source_dir));
    }

    $stmts   = (int) $metrics['statements'];
    $covered = (int) $metrics['coveredstatements'];
    $methods = (int) $metrics['methods'];
    $cov_m   = (int) $metrics['coveredmethods'];
    $classes = (int) $metrics['classes'] ?? 0;
    $cov_c   = (int) $metrics['coveredclasses'] ?? 0;

    $total_statements   += $stmts;
    $covered_statements += $covered;
    $total_methods      += $methods;
    $covered_methods    += $cov_m;
    $total_classes       += $classes;
    $covered_classes     += $cov_c;

    $line_pct = $stmts > 0 ? round(($covered / $stmts) * 100, 2) : 100;

    $files[] = [
        'file'     => $file_path,
        'lines'    => $stmts,
        'covered'  => $covered,
        'line_pct' => $line_pct,
    ];
}
