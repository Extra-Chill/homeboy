#!/usr/bin/env php
<?php
/**
 * Short Ternary Fixer
 *
 * Expands short ternary operators `$var ?: $default` to `$var ? $var : $default`.
 * Only handles simple variable cases to avoid double evaluation issues with function calls.
 *
 * WordPress/Universal coding standards disallow short ternary operators.
 *
 * Usage: php short-ternary-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php short-ternary-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "Short ternary fixer: Fixed {$result['total_fixes']} expression(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "Short ternary fixer: No fixable expressions found\n";
}

exit(0);

/**
 * Process a single PHP file.
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

        // Look for simple expression followed by ?:
        if (is_simple_left_side($token)) {
            $result = try_fix_short_ternary($tokens, $i, $count);
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
 * Check if token starts a simple left side (variable or array access).
 */
function is_simple_left_side($token) {
    return is_array($token) && $token[0] === T_VARIABLE;
}

/**
 * Try to fix a short ternary expression.
 *
 * Handles: $var ?: $default
 * Becomes: $var ? $var : $default
 *
 * Also handles: $arr['key'] ?: $default
 */
function try_fix_short_ternary($tokens, $i, $count) {
    $j = $i;

    // Capture the left side (variable + optional array access)
    $left_side = '';
    $left_side .= $tokens[$j][1];
    $j++;

    // Handle array access
    while ($j < $count) {
        // Skip whitespace
        $ws = '';
        while ($j < $count && is_whitespace($tokens[$j])) {
            $ws .= $tokens[$j][1];
            $j++;
        }

        if ($j >= $count) {
            break;
        }

        // Check for array access
        if ($tokens[$j] === '[') {
            $left_side .= $ws;
            $bracket = capture_brackets($tokens, $j, $count);
            if ($bracket === null) {
                return null;
            }
            $left_side .= $bracket['content'];
            $j = $bracket['end_index'] + 1;
            continue;
        }

        // Check for method call or property access - skip (side effects)
        if (is_array($tokens[$j]) && in_array($tokens[$j][0], [T_OBJECT_OPERATOR, T_NULLSAFE_OBJECT_OPERATOR], true)) {
            return null;
        }

        // Check for function call - skip (side effects)
        if ($tokens[$j] === '(') {
            return null;
        }

        break;
    }

    // Skip whitespace before ?:
    $ws_before_op = '';
    while ($j < $count && is_whitespace($tokens[$j])) {
        $ws_before_op .= $tokens[$j][1];
        $j++;
    }

    if ($j >= $count) {
        return null;
    }

    // Check for ? (first part of ?:)
    if ($tokens[$j] !== '?') {
        return null;
    }
    $j++;

    // Check for : immediately after ? (this is the short ternary pattern)
    // There might be whitespace between ? and :
    $ws_between = '';
    while ($j < $count && is_whitespace($tokens[$j])) {
        $ws_between .= $tokens[$j][1];
        $j++;
    }

    if ($j >= $count || $tokens[$j] !== ':') {
        return null;
    }
    $j++;

    // This is a short ternary! Expand it.
    // $left ?: becomes $left ? $left :

    // Capture whitespace after :
    $ws_after_colon = '';
    while ($j < $count && is_whitespace($tokens[$j])) {
        $ws_after_colon .= $tokens[$j][1];
        $j++;
    }

    // The rest is the default value - we don't need to capture it
    // We just need to output up to and including the colon

    // Build replacement: left_side ? left_side :
    $replacement = $left_side . $ws_before_op . '? ' . $left_side . ' :' . $ws_after_colon;

    // end_index should be just before the default value
    return [
        'replacement' => $replacement,
        'end_index' => $j - 1,
    ];
}

/**
 * Check if token is whitespace.
 */
function is_whitespace($token) {
    return is_array($token) && $token[0] === T_WHITESPACE;
}

/**
 * Capture bracket content.
 */
function capture_brackets($tokens, $start, $count) {
    if ($tokens[$start] !== '[') {
        return null;
    }

    $content = '[';
    $depth = 1;
    $j = $start + 1;

    while ($j < $count && $depth > 0) {
        $token = $tokens[$j];
        if ($token === '[') {
            $depth++;
        } elseif ($token === ']') {
            $depth--;
        }
        $content .= token_to_string($token);
        $j++;
    }

    if ($depth !== 0) {
        return null;
    }

    return [
        'content' => $content,
        'end_index' => $j - 1,
    ];
}

/**
 * Convert token to string.
 */
function token_to_string($token) {
    return is_array($token) ? $token[1] : $token;
}
