<?php
/**
 * Shared helper functions for PHP fixers.
 */

/**
 * Process PHP files in a path, excluding generated dependency/build directories.
 *
 * @param string   $path     File or directory path.
 * @param callable $callback Function to process each file, receives filepath, returns fix count.
 * @return array ['total_fixes' => int, 'files_fixed' => int]
 */
function fixer_process_path($path, callable $callback) {
    $total_fixes = 0;
    $files_fixed = 0;

    if (is_dir($path)) {
        $iterator = new RecursiveIteratorIterator(
            new RecursiveCallbackFilterIterator(
                new RecursiveDirectoryIterator($path, RecursiveDirectoryIterator::SKIP_DOTS),
                function ($file, $key, $iterator) {
                    if ($iterator->hasChildren()) {
                        return !fixer_path_is_excluded($file->getPathname());
                    }
                    return $file->getExtension() === 'php';
                }
            )
        );

        foreach ($iterator as $file) {
            $fixes = $callback($file->getPathname());
            if ($fixes > 0) {
                $files_fixed++;
                $total_fixes += $fixes;
            }
        }
    } else {
        $total_fixes = $callback($path);
        if ($total_fixes > 0) {
            $files_fixed = 1;
        }
    }

    return ['total_fixes' => $total_fixes, 'files_fixed' => $files_fixed];
}

function fixer_path_is_excluded($path) {
    $path = str_replace('\\', '/', $path);
    $excluded_dirs = [
        'vendor',
        'vendor_prefixed',
        'vendor-prefixed',
        'vendor_scoped',
        'vendor-scoped',
        'node_extensions',
        'node_modules',
        'dist',
        'build',
    ];

    foreach (explode('/', trim($path, '/')) as $segment) {
        if (in_array($segment, $excluded_dirs, true)) {
            return true;
        }
    }

    return false;
}
