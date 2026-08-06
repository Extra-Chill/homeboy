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
    } elseif (pathinfo($path, PATHINFO_EXTENSION) === 'php') {
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

/**
 * Check whether a fixer target is explicitly suppressed for PHPCS.
 *
 * @param array $lines Source lines.
 * @param int   $line_index Zero-based target line index.
 * @return bool True when the target or its preceding line has a suppression.
 */
function fixer_line_has_phpcs_ignore(array $lines, $line_index) {
    foreach ([$line_index - 1, $line_index] as $index) {
        if (isset($lines[$index]) && strpos($lines[$index], 'phpcs:ignore') !== false) {
            return true;
        }
    }

    return false;
}

/**
 * Check whether any line in a code region explicitly suppresses PHPCS.
 *
 * @param array $lines Source lines.
 * @return bool True when the region contains a suppression.
 */
function fixer_lines_have_phpcs_ignore(array $lines) {
    foreach ($lines as $line) {
        if (strpos($line, 'phpcs:ignore') !== false) {
            return true;
        }
    }

    return false;
}

/**
 * Detect pure-PHP test harness files that don't bootstrap WordPress.
 *
 * Smoke tests (`tests/*-smoke.php`, `tests/smoke-*.php`) and PHPUnit test classes
 * (`tests/*Test.php`, `tests/*TestCase.php`) frequently do filesystem I/O without
 * a WP runtime. Auto-fixers that rewrite to WordPress runtime APIs (e.g.
 * $wp_filesystem) MUST skip these files — the rewritten code would throw at
 * runtime because $wp_filesystem is undefined.
 *
 * Mirrors the role detection in scripts/lint/lint-runner.sh
 * (wordpress_lint_role_for_path: smoke_harness, phpunit_test).
 *
 * @param string $path Absolute or relative path to a PHP file.
 * @return bool True if the file is a pure-PHP test harness.
 */
function fixer_path_is_test_harness($path) {
    $path = str_replace('\\', '/', $path);
    $basename = basename($path);

    // PHPUnit test classes: tests/*Test.php, tests/*TestCase.php (any depth).
    if (preg_match('#(^|/)tests/.*(Test|TestCase)\.php$#', $path)) {
        return true;
    }

    // Smoke harnesses: tests/*-smoke.php, tests/smoke-*.php (any depth).
    if (preg_match('#(^|/)tests/#', $path)) {
        if (preg_match('/(^|-)smoke\.php$/', $basename) || preg_match('/^smoke-/', $basename)) {
            return true;
        }
    }

    // *-smoke.php anywhere — matches the lint role pattern */smoke-*.php and
    // */*-smoke.php from wordpress_lint_role_for_path.
    if (preg_match('/-smoke\.php$/', $basename) || preg_match('/^smoke-/', $basename)) {
        return true;
    }

    return false;
}
