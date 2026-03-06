#!/usr/bin/env php
<?php
/**
 * Short Ternary Fixer
 *
 * Expands short ternary operators `expr ?: $default` to `expr ? expr : $default`.
 * Handles variables, array access, property access, method calls, and function calls.
 *
 * Note: For function/method call expressions, the expansion results in double evaluation.
 * This is accepted as the standard WordPress/WPCS pattern for removing short ternaries.
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
 * Process a single PHP file using regex-based ?:-detection + token-based expansion.
 *
 * Strategy: find ?: positions via simple string scan, then use token analysis
 * to extract the left-side expression and expand it.
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

    // Track expression start candidates as we walk forward.
    // When we see ?: we look back to find the start of the expression.
    while ($i < $count) {
        $token = $tokens[$i];

        // Detect ?: pattern: '?' followed by ':' (with optional whitespace)
        if ($token === '?') {
            $peek = $i + 1;
            $ws_between = '';
            while ($peek < $count && is_whitespace($tokens[$peek])) {
                $ws_between .= $tokens[$peek][1];
                $peek++;
            }

            if ($peek < $count && $tokens[$peek] === ':') {
                // Found ?: — try to extract the left expression from new_content
                $result = expand_short_ternary($new_content, $tokens, $peek, $count);
                if ($result !== null) {
                    $new_content = $result['new_content'];
                    $i = $result['resume_index'];
                    $fixes++;
                    continue;
                }
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
 * Expand a short ternary by extracting the left expression from already-emitted content.
 *
 * @param string $emitted   Content emitted so far (will be modified to include expansion).
 * @param array  $tokens    Token array.
 * @param int    $colon_idx Index of the ':' token in the ?: pattern.
 * @param int    $count     Total token count.
 * @return array|null ['new_content' => string, 'resume_index' => int] or null.
 */
function expand_short_ternary($emitted, $tokens, $colon_idx, $count) {
    // Extract the left-side expression from already-emitted content.
    // We need to find where the expression starts by walking backward through $emitted.
    $expr = extract_trailing_expression($emitted);
    if ($expr === null || $expr['expression'] === '') {
        return null;
    }

    $left_expr = $expr['expression'];
    $prefix = $expr['prefix'];

    // Skip whitespace after ':'
    $j = $colon_idx + 1;
    $ws_after_colon = '';
    while ($j < $count && is_whitespace($tokens[$j])) {
        $ws_after_colon .= $tokens[$j][1];
        $j++;
    }

    // Capture trailing whitespace before ?: (already in emitted, strip it from left_expr)
    $left_trimmed = rtrim($left_expr);
    $ws_before_qmark = substr($left_expr, strlen($left_trimmed));
    $left_expr = $left_trimmed;

    if ($ws_before_qmark === '') {
        $ws_before_qmark = ' ';
    }

    // Build expanded ternary: prefix + left_expr + ? left_expr : + rest
    $expanded = $prefix . $left_expr . $ws_before_qmark . '? ' . $left_expr . ' :' . $ws_after_colon;

    return [
        'new_content' => $expanded,
        'resume_index' => $j,
    ];
}

/**
 * Extract trailing expression from emitted content string.
 *
 * Walks backward through the string to find a complete PHP expression.
 * Handles: variables, array access, property chains, method calls, function calls,
 * and chained combinations of all.
 *
 * @param string $emitted The content emitted so far.
 * @return array|null ['prefix' => string, 'expression' => string] or null.
 */
function extract_trailing_expression($emitted) {
    $len = strlen($emitted);
    if ($len === 0) {
        return null;
    }

    // Find the end of the expression (skip trailing whitespace)
    $end = $len - 1;
    while ($end >= 0 && ($emitted[$end] === ' ' || $emitted[$end] === "\t")) {
        $end--;
    }

    if ($end < 0) {
        return null;
    }

    $trailing_ws = substr($emitted, $end + 1);
    $pos = $end;

    // Walk backward to capture the expression
    $pos = walk_back_expression($emitted, $pos);

    if ($pos === null || $pos === $end) {
        return null;
    }

    $expr_start = $pos + 1;
    $expression = substr($emitted, $expr_start, $end - $expr_start + 1) . $trailing_ws;
    $prefix = substr($emitted, 0, $expr_start);

    // Validate: expression must start with $ or a function/class name
    $trimmed = ltrim($expression);
    if ($trimmed === '' || (!preg_match('/^[\$a-zA-Z_\\\\]/', $trimmed))) {
        return null;
    }

    return [
        'prefix' => $prefix,
        'expression' => $expression,
    ];
}

/**
 * Walk backward through a string to find the start of a PHP expression.
 *
 * @param string $str The string to walk through.
 * @param int    $pos Current position (at end of expression).
 * @return int|null Position just before the expression starts, or null on failure.
 */
function walk_back_expression($str, $pos) {
    if ($pos < 0) {
        return null;
    }

    $original_pos = $pos;

    // Handle closing paren — balanced walk back for function call args
    if ($str[$pos] === ')') {
        $pos = find_matching_open($str, $pos, '(', ')');
        if ($pos === null) {
            return null;
        }
        $pos--; // Move before the '('

        // Skip whitespace before '('
        while ($pos >= 0 && ($str[$pos] === ' ' || $str[$pos] === "\t")) {
            $pos--;
        }

        if ($pos < 0) {
            return null;
        }

        // The thing before '(' should be a function/method name or closing bracket/paren
        // Recurse to capture it
        return walk_back_expression($str, $pos);
    }

    // Handle closing bracket — array access
    if ($str[$pos] === ']') {
        $pos = find_matching_open($str, $pos, '[', ']');
        if ($pos === null) {
            return null;
        }
        $pos--; // Move before the '['

        // Skip whitespace before '['
        while ($pos >= 0 && ($str[$pos] === ' ' || $str[$pos] === "\t")) {
            $pos--;
        }

        if ($pos < 0) {
            return $pos;
        }

        // Recurse to capture what's before the bracket
        return walk_back_expression($str, $pos);
    }

    // Handle identifier (variable name, function name, property name, class name)
    if (preg_match('/[a-zA-Z0-9_]/', $str[$pos])) {
        while ($pos >= 0 && preg_match('/[a-zA-Z0-9_]/', $str[$pos])) {
            $pos--;
        }

        // Check for $ prefix (variable)
        if ($pos >= 0 && $str[$pos] === '$') {
            $pos--;

            // Check for -> or ?-> before $this or other var (shouldn't happen, but guard)
            $check = $pos;
            while ($check >= 0 && ($str[$check] === ' ' || $str[$check] === "\t")) {
                $check--;
            }

            // Check for -> or ?->
            if ($check >= 1 && $str[$check - 1] === '-' && $str[$check] === '>') {
                $pos = $check - 2;
                while ($pos >= 0 && ($str[$pos] === ' ' || $str[$pos] === "\t")) {
                    $pos--;
                }
                return walk_back_expression($str, $pos);
            }

            // Check for ?->
            if ($check >= 2 && $str[$check - 2] === '?' && $str[$check - 1] === '-' && $str[$check] === '>') {
                $pos = $check - 3;
                while ($pos >= 0 && ($str[$pos] === ' ' || $str[$pos] === "\t")) {
                    $pos--;
                }
                return walk_back_expression($str, $pos);
            }

            return $pos;
        }

        // Non-variable identifier: function name, property name, class name, constant
        // Check what precedes it

        $check = $pos;
        while ($check >= 0 && ($str[$check] === ' ' || $str[$check] === "\t")) {
            $check--;
        }

        // Check for -> (property/method access)
        if ($check >= 1 && $str[$check - 1] === '-' && $str[$check] === '>') {
            $pos = $check - 2;
            while ($pos >= 0 && ($str[$pos] === ' ' || $str[$pos] === "\t")) {
                $pos--;
            }
            return walk_back_expression($str, $pos);
        }

        // Check for ?-> (nullsafe operator)
        if ($check >= 2 && $str[$check - 2] === '?' && $str[$check - 1] === '-' && $str[$check] === '>') {
            $pos = $check - 3;
            while ($pos >= 0 && ($str[$pos] === ' ' || $str[$pos] === "\t")) {
                $pos--;
            }
            return walk_back_expression($str, $pos);
        }

        // Check for :: (static access)
        if ($check >= 1 && $str[$check - 1] === ':' && $str[$check] === ':') {
            $pos = $check - 2;
            // Capture the class name before ::
            while ($pos >= 0 && preg_match('/[a-zA-Z0-9_\\\\]/', $str[$pos])) {
                $pos--;
            }
            return $pos;
        }

        // Standalone function name or keyword — this is the start of the expression
        return $pos;
    }

    // Nothing we recognize
    return null;
}

/**
 * Find matching opening bracket/paren by walking backward.
 *
 * @param string $str   The string.
 * @param int    $pos   Position of closing bracket.
 * @param string $open  Opening character.
 * @param string $close Closing character.
 * @return int|null Position of matching opening bracket, or null.
 */
function find_matching_open($str, $pos, $open, $close) {
    $depth = 1;
    $pos--;

    while ($pos >= 0 && $depth > 0) {
        if ($str[$pos] === $close) {
            $depth++;
        } elseif ($str[$pos] === $open) {
            $depth--;
        }
        if ($depth > 0) {
            $pos--;
        }
    }

    return $depth === 0 ? $pos : null;
}

/**
 * Check if token is whitespace.
 */
function is_whitespace($token) {
    return is_array($token) && $token[0] === T_WHITESPACE;
}

/**
 * Convert token to string.
 */
function token_to_string($token) {
    return is_array($token) ? $token[1] : $token;
}
