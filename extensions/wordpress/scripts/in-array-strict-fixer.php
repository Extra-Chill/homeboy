#!/usr/bin/env php
<?php
/**
 * in_array Strict Mode Fixer
 *
 * Adds `true` as the third parameter to in_array() calls that are missing it.
 * WordPress coding standards require strict mode for in_array().
 *
 * Usage: php in-array-strict-fixer.php <path>
 */

if ($argc < 2) {
    echo "Usage: php in-array-strict-fixer.php <path>\n";
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
    echo "in_array strict fixer: Fixed $total_fixes call(s) in $files_fixed file(s)\n";
} else {
    echo "in_array strict fixer: No fixable calls found\n";
}

exit(0);

/**
 * Process a single PHP file and fix in_array calls.
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

        // Look for in_array function call
        if (is_array($token) && $token[0] === T_STRING && strtolower($token[1]) === 'in_array') {
            $result = try_fix_in_array($tokens, $i, $count);
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
 * Try to fix an in_array call by adding strict parameter.
 */
function try_fix_in_array($tokens, $i, $count) {
    $function_name = $tokens[$i][1];
    $j = $i + 1;

    // Skip whitespace
    while ($j < $count && is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
        $j++;
    }

    // Must be followed by opening parenthesis
    if ($j >= $count || $tokens[$j] !== '(') {
        return null;
    }

    $open_paren_index = $j;
    $j++;

    // Track parenthesis depth and find arguments
    $depth = 1;
    $arg_count = 0;
    $last_comma_index = null;
    $arg_start = $j;
    $has_content = false;

    while ($j < $count && $depth > 0) {
        $token = $tokens[$j];

        if ($token === '(') {
            $depth++;
            $has_content = true;
        } elseif ($token === ')') {
            $depth--;
            if ($depth === 0) {
                // End of function call
                if ($has_content) {
                    $arg_count++;
                }
                break;
            }
        } elseif ($token === ',' && $depth === 1) {
            $arg_count++;
            $last_comma_index = $j;
            $has_content = false;
        } elseif ($token === '[') {
            $depth++;
            $has_content = true;
        } elseif ($token === ']') {
            $depth--;
            $has_content = true;
        } elseif (!is_array($token) || $token[0] !== T_WHITESPACE) {
            $has_content = true;
        }

        $j++;
    }

    $close_paren_index = $j;

    // Only fix if we have exactly 2 arguments (needle, haystack)
    if ($arg_count !== 2) {
        return null;
    }

    // Build replacement: original content + ", true"
    $replacement = '';
    for ($k = $i; $k < $close_paren_index; $k++) {
        $replacement .= token_to_string($tokens[$k]);
    }
    $replacement .= ', true)';

    return [
        'replacement' => $replacement,
        'end_index' => $close_paren_index,
    ];
}

/**
 * Convert token to string.
 */
function token_to_string($token) {
    return is_array($token) ? $token[1] : $token;
}
