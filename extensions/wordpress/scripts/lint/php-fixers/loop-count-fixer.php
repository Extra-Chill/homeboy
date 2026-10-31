#!/usr/bin/env php
<?php
/**
 * Loop Count Hoister Fixer
 *
 * Extracts count()/sizeof() calls from for-loop conditions into variables.
 *
 * Before: for ($i = 0; $i < count($arr); $i++)
 * After:  $arr_count = count($arr);
 *         for ($i = 0; $i < $arr_count; $i++)
 *
 * Usage: php loop-count-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php loop-count-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "Loop count fixer: Fixed {$result['total_fixes']} loop(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "Loop count fixer: No fixable loops found\n";
}

exit(0);

/**
 * Process a single PHP file using line-based regex.
 *
 * This uses a multi-line regex approach since for-loop conditions are typically
 * on a single line and the transform is straightforward.
 */
function process_file($filepath) {
    $content = file_get_contents($filepath);
    if ($content === false) {
        return 0;
    }

    $fixes = 0;

    // Match for loops with count()/sizeof() in the condition
    // Pattern: for ( init; condition_with_count(...); increment )
    $pattern = '/^(\h*)(for\s*\(\s*.*?;\s*)(.*?\b(count|sizeof)\s*\(\s*(.+?)\s*\))(.*?;\s*.*?\))/m';

    $new_content = preg_replace_callback($pattern, function ($matches) use (&$fixes) {
        $indent = $matches[1];
        $for_prefix = $matches[2];       // "for ( $i = 0; "
        $condition = $matches[3];         // "$i < count($arr) - 1" etc.
        $func = $matches[4];             // "count" or "sizeof"
        $arg = $matches[5];              // the argument to count()
        $for_suffix = $matches[6];       // "; $i++ )"

        // Build variable name from the argument
        $var_name = build_var_name($arg, $func);
        $count_call = $func . '( ' . $arg . ' )';

        // Replace the count() call in the condition with the variable
        $new_condition = str_replace($matches[4] . '(' . $matches[5] . ')', '$' . $var_name, $condition);

        // Hmm, the regex captured groups don't nest well. Let me use a simpler approach.
        // Just replace the count()/sizeof() call in the condition with the variable.
        $count_expr = $func . '( ' . $arg . ' )';
        // Try both with and without spaces
        $count_patterns = [
            $func . '( ' . $arg . ' )',
            $func . '(' . $arg . ')',
            $func . '( ' . $arg . ')',
            $func . '(' . $arg . ' )',
        ];

        $replaced = false;
        $new_cond = $condition;
        foreach ($count_patterns as $cp) {
            if (strpos($new_cond, $cp) !== false) {
                $new_cond = str_replace($cp, '$' . $var_name, $new_cond);
                $count_expr = $cp;
                $replaced = true;
                break;
            }
        }

        if (!$replaced) {
            return $matches[0]; // Can't find the count call to replace
        }

        $fixes++;

        // Build the variable declaration line before the for loop
        $var_line = $indent . '$' . $var_name . ' = ' . $count_expr . ";\n";

        return $var_line . $indent . $for_prefix . $new_cond . $for_suffix;
    }, $content);

    if ($new_content === null || $fixes === 0) {
        return 0;
    }

    file_put_contents($filepath, $new_content);
    return $fixes;
}

/**
 * Build a variable name from a count() argument.
 *
 * count($arr) → arr_count
 * count($image_positions) → image_positions_count
 * sizeof($items) → items_count
 */
function build_var_name($arg, $func) {
    $arg = trim($arg);

    // Strip $ prefix
    if (strpos($arg, '$') === 0) {
        $name = substr($arg, 1);
    } else {
        $name = $arg;
    }

    // Clean up: remove array access, method calls
    $name = preg_replace('/[\[\(].*/', '', $name);
    $name = preg_replace('/->.*/', '', $name);

    // Sanitize
    $name = preg_replace('/[^a-zA-Z0-9_]/', '', $name);

    if (empty($name)) {
        $name = 'item';
    }

    return $name . '_count';
}
