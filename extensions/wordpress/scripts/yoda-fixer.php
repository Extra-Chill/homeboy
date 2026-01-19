#!/usr/bin/env php
<?php
/**
 * Yoda Condition Fixer
 *
 * Safely swaps operands in common Yoda violation patterns using PHP tokenization.
 * WPCS 3.3.0 deliberately uses addError() instead of addFixableError() for Yoda
 * conditions, making this a differentiating capability for Homeboy.
 *
 * Usage: php yoda-fixer.php <path>
 */

if ($argc < 2) {
    echo "Usage: php yoda-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$total_fixes = 0;
$files_fixed = 0;

if (is_dir($path)) {
    $iterator = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($path, RecursiveDirectoryIterator::SKIP_DOTS)
    );

    foreach ($iterator as $file) {
        if ($file->getExtension() === 'php') {
            $fixes = process_file($file->getPathname());
            if ($fixes > 0) {
                $files_fixed++;
                $total_fixes += $fixes;
            }
        }
    }
} else {
    $total_fixes = process_file($path);
    if ($total_fixes > 0) {
        $files_fixed = 1;
    }
}

if ($total_fixes > 0) {
    echo "Yoda fixer: Fixed $total_fixes condition(s) in $files_fixed file(s)\n";
} else {
    echo "Yoda fixer: No fixable conditions found\n";
}

exit(0);

/**
 * Process a single PHP file and fix Yoda conditions.
 *
 * @param string $filepath Path to PHP file.
 * @return int Number of fixes made.
 */
function process_file($filepath) {
    $content = file_get_contents($filepath);
    if ($content === false) {
        return 0;
    }

    $tokens = @token_get_all($content);
    if ($tokens === false) {
        return 0;
    }

    $fixes = 0;
    $new_content = '';
    $i = 0;
    $count = count($tokens);

    while ($i < $count) {
        $token = $tokens[$i];

        // Check for comparison pattern: $var COMPARISON_OP literal
        if (is_variable_token($token)) {
            $result = try_fix_yoda($tokens, $i, $count);
            if ($result !== null) {
                $new_content .= $result['replacement'];
                $i = $result['end_index'] + 1;
                $fixes++;
                continue;
            }
        }

        $new_content .= token_to_string($token);
        $i++;
    }

    if ($fixes > 0) {
        file_put_contents($filepath, $new_content);
    }

    return $fixes;
}

/**
 * Check if token is a variable.
 */
function is_variable_token($token) {
    return is_array($token) && $token[0] === T_VARIABLE;
}

/**
 * Check if token is a comparison operator.
 */
function is_comparison_op($token) {
    if (!is_array($token)) {
        return false;
    }

    return in_array($token[0], [
        T_IS_IDENTICAL,     // ===
        T_IS_NOT_IDENTICAL, // !==
        T_IS_EQUAL,         // ==
        T_IS_NOT_EQUAL,     // !=
    ], true);
}

/**
 * Check if token is a simple literal (string, number, null, true, false).
 */
function is_simple_literal($token) {
    if (!is_array($token)) {
        return false;
    }

    // PHP 8+ uses T_NAME_* for true/false/null
    $literal_types = [
        T_CONSTANT_ENCAPSED_STRING, // 'string' or "string"
        T_LNUMBER,                  // Integer
        T_DNUMBER,                  // Float
    ];

    // Handle true/false/null across PHP versions
    if (defined('T_NAME_FULLY_QUALIFIED')) {
        $literal_types[] = T_NAME_FULLY_QUALIFIED;
        $literal_types[] = T_NAME_QUALIFIED;
        $literal_types[] = T_NAME_RELATIVE;
    }

    if (in_array($token[0], $literal_types, true)) {
        return true;
    }

    // T_STRING for true/false/null in older PHP or class constants
    if ($token[0] === T_STRING) {
        $lower = strtolower($token[1]);
        return in_array($lower, ['true', 'false', 'null'], true);
    }

    return false;
}

/**
 * Check if token is whitespace (not containing newlines).
 */
function is_inline_whitespace($token) {
    if (!is_array($token)) {
        return false;
    }

    return $token[0] === T_WHITESPACE && strpos($token[1], "\n") === false;
}

/**
 * Try to fix a Yoda condition starting at index $i.
 *
 * @param array $tokens Token array.
 * @param int   $i      Current index (at variable token).
 * @param int   $count  Total token count.
 * @return array|null Result with 'replacement' and 'end_index', or null if not fixable.
 */
function try_fix_yoda($tokens, $i, $count) {
    $var_token = $tokens[$i];
    $var_str = $var_token[1];
    $j = $i + 1;

    // Skip whitespace after variable
    $ws_after_var = '';
    while ($j < $count && is_inline_whitespace($tokens[$j])) {
        $ws_after_var .= $tokens[$j][1];
        $j++;
    }

    if ($j >= $count) {
        return null;
    }

    // Check for comparison operator
    if (!is_comparison_op($tokens[$j])) {
        return null;
    }
    $op_token = $tokens[$j];
    $op_str = $op_token[1];
    $j++;

    // Skip whitespace after operator
    $ws_after_op = '';
    while ($j < $count && is_inline_whitespace($tokens[$j])) {
        $ws_after_op .= $tokens[$j][1];
        $j++;
    }

    if ($j >= $count) {
        return null;
    }

    // Check for simple literal on right side
    if (!is_simple_literal($tokens[$j])) {
        return null;
    }
    $literal_token = $tokens[$j];
    $literal_str = $literal_token[1];

    // Check next token to ensure we're not in a complex expression
    $k = $j + 1;
    while ($k < $count && is_inline_whitespace($tokens[$k])) {
        $k++;
    }

    if ($k < $count) {
        $next = $tokens[$k];
        // Skip if followed by object operator, array access, or arithmetic
        if (is_array($next)) {
            if (in_array($next[0], [T_OBJECT_OPERATOR, T_NULLSAFE_OBJECT_OPERATOR], true)) {
                return null;
            }
        } elseif (in_array($next, ['[', '+', '-', '*', '/', '%', '.'], true)) {
            return null;
        }
    }

    // Build the swapped comparison: literal OPERATOR $var
    $replacement = $literal_str . $ws_after_var . $op_str . $ws_after_op . $var_str;

    return [
        'replacement' => $replacement,
        'end_index' => $j,
    ];
}

/**
 * Convert token to string.
 */
function token_to_string($token) {
    return is_array($token) ? $token[1] : $token;
}
