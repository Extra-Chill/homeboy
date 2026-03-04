<?php
/**
 * Parse PHPUnit output into structured failure data for homeboy test --analyze.
 *
 * Reads PHPUnit output from a file, extracts individual test failures with:
 * - test_name: fully qualified test method name
 * - test_file: test file path (from stack trace)
 * - error_type: exception/error class name
 * - message: error message
 * - source_file: deepest non-test frame source file
 * - source_line: source line number
 *
 * Outputs JSON matching homeboy's TestAnalysisInput schema.
 *
 * Usage: php parse-test-failures.php <phpunit_output_file> [component_path]
 */

if ($argc < 2) {
    fwrite(STDERR, "Usage: php parse-test-failures.php <phpunit_output_file> [component_path]\n");
    exit(1);
}

$output_file = $argv[1];
$component_path = $argc >= 3 ? rtrim($argv[2], '/') . '/' : '';

if (!file_exists($output_file)) {
    fwrite(STDERR, "File not found: $output_file\n");
    exit(1);
}

$raw = file_get_contents($output_file);
$lines = explode("\n", $raw);

$failures = [];
$total = 0;
$passed = 0;

// ============================================================================
// Phase 1: Extract test counts from summary line
// ============================================================================

// Success: "OK (N tests, N assertions)"
if (preg_match('/OK \((\d+) tests?/', $raw, $m)) {
    $total = (int) $m[1];
    $passed = $total;
}
// Failure: "Tests: N, Assertions: N, Errors: N, Failures: N, Skipped: N."
elseif (preg_match('/^Tests:\s*(\d+)/m', $raw, $m)) {
    $total = (int) $m[1];
    $errors = 0;
    $failed_count = 0;
    $skipped = 0;
    if (preg_match('/Errors:\s*(\d+)/', $raw, $em)) $errors = (int) $em[1];
    if (preg_match('/Failures:\s*(\d+)/', $raw, $fm)) $failed_count = (int) $fm[1];
    if (preg_match('/Skipped:\s*(\d+)/', $raw, $sm)) $skipped = (int) $sm[1];
    $passed = max(0, $total - $errors - $failed_count - $skipped);
}

// ============================================================================
// Phase 2: Find failure/error blocks
// ============================================================================
// PHPUnit formats failures as numbered blocks:
//
//   1) Namespace\ClassTest::testMethod
//   Error message here
//   possibly multiple lines
//
//   /path/to/file.php:42
//   /path/to/other.php:10
//
//   2) Namespace\ClassTest::testOther
//   ...
//
// Fatal errors appear as:
//   1) Namespace\ClassTest::testMethod
//   Cannot redeclare function_name() (previously declared in /path:42) in /path:42
//
// Sections are delimited by "There was N error(s):" / "There was N failure(s):"
// or by "ERRORS!" / "FAILURES!" banners.

// Split into failure blocks. Each block starts with "N) " at line start.
$in_failure_section = false;
$current_block = null;
$blocks = [];

for ($i = 0; $i < count($lines); $i++) {
    $line = $lines[$i];

    // Detect start of failure/error listing sections
    if (preg_match('/^There (?:was|were) \d+ (?:error|failure)/i', $line)) {
        $in_failure_section = true;
        continue;
    }

    // Detect end markers
    if (preg_match('/^(ERRORS!|FAILURES!)/', $line)) {
        $in_failure_section = false;
        // Save last block
        if ($current_block !== null) {
            $blocks[] = $current_block;
            $current_block = null;
        }
        continue;
    }

    // Summary line ends all blocks
    if (preg_match('/^Tests:\s*\d+/', $line)) {
        if ($current_block !== null) {
            $blocks[] = $current_block;
            $current_block = null;
        }
        $in_failure_section = false;
        continue;
    }

    if (!$in_failure_section) {
        continue;
    }

    // New numbered block: "N) Namespace\ClassTest::testMethod"
    if (preg_match('/^\d+\)\s+(.+)$/', $line, $m)) {
        // Save previous block
        if ($current_block !== null) {
            $blocks[] = $current_block;
        }
        $current_block = [
            'header' => trim($m[1]),
            'body_lines' => [],
        ];
        continue;
    }

    // Accumulate body lines for current block
    if ($current_block !== null) {
        $current_block['body_lines'][] = $line;
    }
}

// Don't forget last block
if ($current_block !== null) {
    $blocks[] = $current_block;
}

// ============================================================================
// Phase 3: Parse each block into a TestFailure
// ============================================================================

foreach ($blocks as $block) {
    $header = $block['header'];
    $body = $block['body_lines'];

    // Parse test name from header
    // Format: "Namespace\ClassTest::testMethod" or "Namespace\ClassTest::testMethod with data set #0"
    $test_name = $header;
    if (strpos($test_name, ' with data set') !== false) {
        $test_name = substr($test_name, 0, strpos($test_name, ' with data set'));
    }

    // Build message from body lines (non-empty, non-trace lines)
    $message_lines = [];
    $trace_lines = [];
    $in_trace = false;

    foreach ($body as $bline) {
        $trimmed = trim($bline);

        // Skip empty lines between message and trace
        if ($trimmed === '') {
            if (!empty($message_lines) && !$in_trace) {
                // Could be transitioning to trace section
                continue;
            }
            continue;
        }

        // Stack trace lines start with a path
        if (preg_match('#^(/[^\s:]+\.php):(\d+)$#', $trimmed) ||
            preg_match('#^(/[^\s:]+\.php:\d+)$#', $trimmed)) {
            $in_trace = true;
            $trace_lines[] = $trimmed;
            continue;
        }

        // Also match indented trace format "at /path/to/file.php:42"
        if (preg_match('#^\s*at\s+(/[^\s:]+\.php):(\d+)#', $bline)) {
            $in_trace = true;
            $trace_lines[] = $trimmed;
            continue;
        }

        if (!$in_trace) {
            $message_lines[] = $trimmed;
        }
    }

    $message = implode("\n", $message_lines);

    // Trim trailing empty content from message
    $message = rtrim($message);

    // ---- Detect error type ----
    $error_type = 'Error';

    // Check for fatal error patterns
    if (preg_match('/^(Fatal error|PHP Fatal error):/i', $message)) {
        $error_type = 'FatalError';
    }
    // "Cannot redeclare ..." is a fatal error
    elseif (preg_match('/^Cannot redeclare\b/', $message)) {
        $error_type = 'FatalError';
    }
    // PHPUnit assertion failures
    elseif (preg_match('/^Failed asserting/', $message)) {
        $error_type = 'AssertionFailedError';
    }
    // "Call to undefined method" — Error
    elseif (preg_match('/^Call to undefined method/', $message)) {
        $error_type = 'Error';
    }
    // "Call to undefined function" — Error
    elseif (preg_match('/^Call to undefined function/', $message)) {
        $error_type = 'Error';
    }
    // "Class .* not found"
    elseif (preg_match('/^Class .* not found/', $message)) {
        $error_type = 'Error';
    }
    // "Argument #N ... must be of type X, Y given"
    elseif (preg_match('/must be of type/', $message)) {
        $error_type = 'TypeError';
    }
    // "Return value .* must be of type"
    elseif (preg_match('/Return value .* must be of type/', $message)) {
        $error_type = 'TypeError';
    }
    // PHPUnit mock errors
    elseif (preg_match('/Trying to configure method .* which cannot be configured/', $message)) {
        $error_type = 'MockError';
    }
    elseif (preg_match('/does not allow named arguments/', $message)) {
        $error_type = 'MockError';
    }
    // Check message for explicit exception class prefix like "ErrorException:"
    elseif (preg_match('/^(\w+(?:\\\\\w+)*(?:Error|Exception)):\s/', $message, $em)) {
        $error_type = $em[1];
    }

    // ---- Extract source file from trace (deepest non-test frame) ----
    $source_file = '';
    $source_line = 0;
    $test_file = '';

    foreach ($trace_lines as $tline) {
        if (preg_match('#^(/[^\s:]+\.php):(\d+)#', $tline, $tm)) {
            $file = $tm[1];
            $line_num = (int) $tm[2];

            // Strip component_path prefix for relative paths
            $rel_file = $file;
            if ($component_path && strpos($file, $component_path) === 0) {
                $rel_file = substr($file, strlen($component_path));
            }

            // Test files contain /tests/ in their path
            if (strpos($file, '/tests/') !== false || strpos($file, 'Test.php') !== false) {
                if (empty($test_file)) {
                    $test_file = $rel_file;
                }
            } else {
                // Non-test file — this is a source file
                // We want the deepest (first) non-test frame
                if (empty($source_file)) {
                    $source_file = $rel_file;
                    $source_line = $line_num;
                }
            }
        }
    }

    // If no test file found from trace, try to extract from the test_name
    if (empty($test_file) && preg_match('/^(.+)::/', $test_name, $nm)) {
        $class_fqn = $nm[1];
        // Convert namespace to path guess: Namespace\SubTest -> tests/Namespace/SubTest.php
        $test_file = 'tests/' . str_replace('\\', '/', $class_fqn) . '.php';
    }

    // Also try extracting source from the message itself for fatal errors
    // e.g., "Cannot redeclare func() (previously declared in /path/to/file.php:42)"
    if (empty($source_file) && preg_match('#in (/[^\s:]+\.php):(\d+)#', $message, $mm)) {
        $file = $mm[1];
        $line_num = (int) $mm[2];
        $rel_file = $file;
        if ($component_path && strpos($file, $component_path) === 0) {
            $rel_file = substr($file, strlen($component_path));
        }
        if (strpos($file, '/tests/') === false && strpos($file, 'Test.php') === false) {
            $source_file = $rel_file;
            $source_line = $line_num;
        }
    }

    $failures[] = [
        'test_name' => $test_name,
        'test_file' => $test_file,
        'error_type' => $error_type,
        'message' => $message,
        'source_file' => $source_file,
        'source_line' => $source_line,
    ];
}

// ============================================================================
// Output
// ============================================================================

$output = [
    'failures' => $failures,
    'total' => $total,
    'passed' => $passed,
];

echo json_encode($output, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES) . "\n";
