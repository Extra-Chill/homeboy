#!/usr/bin/env php
<?php
/**
 * Reserved Keyword Parameter Name Fixer
 *
 * Renames function parameters that use PHP reserved keywords:
 *   $default → $default_value
 *   $class   → $class_name
 *   $parent  → $parent_item
 *   $null    → $null_value
 *
 * Also updates all usages of the renamed parameter within the function body.
 *
 * Usage: php reserved-param-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php reserved-param-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "Reserved param fixer: Fixed {$result['total_fixes']} parameter(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "Reserved param fixer: No fixable parameters found\n";
}

exit(0);

/**
 * Get the mapping of reserved keywords to safe replacements.
 *
 * @return array<string, string>
 */
function get_reserved_param_map() {
    return [
        'default'  => 'default_value',
        'class'    => 'class_name',
        'parent'   => 'parent_item',
        'null'     => 'null_value',
        'list'     => 'list_items',
        'match'    => 'match_value',
        'array'    => 'array_value',
        'string'   => 'string_value',
        'int'      => 'int_value',
        'float'    => 'float_value',
        'bool'     => 'bool_value',
        'object'   => 'object_value',
        'callable' => 'callable_fn',
        'fn'       => 'fn_callback',
        'enum'     => 'enum_value',
        'switch'   => 'switch_value',
        'return'   => 'return_value',
        'print'    => 'print_value',
        'echo'     => 'echo_value',
        'include'  => 'include_path',
        'require'  => 'require_path',
        'static'   => 'static_value',
        'final'    => 'final_value',
        'abstract' => 'abstract_value',
        'interface' => 'interface_name',
        'trait'    => 'trait_name',
        'global'   => 'global_value',
        'var'      => 'var_value',
    ];
}

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

    $count = count($tokens);
    $fixes = 0;

    // Find function/method declarations and check their parameters
    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i])) {
            continue;
        }

        if ($tokens[$i][0] !== T_FUNCTION) {
            continue;
        }

        // Found a function declaration. Find the parameter list.
        $paren_open = find_next_token($tokens, $i + 1, $count, '(');
        if ($paren_open === null) {
            continue;
        }

        $paren_close = find_matching_paren($tokens, $paren_open, $count);
        if ($paren_close === null) {
            continue;
        }

        // Extract parameter names from the parameter list
        $renames = find_reserved_params($tokens, $paren_open, $paren_close);
        if (empty($renames)) {
            continue;
        }

        // Find the function body
        $body_open = find_next_token($tokens, $paren_close + 1, $count, '{');
        if ($body_open === null) {
            continue; // Abstract method or interface
        }

        $body_close = find_matching_brace($tokens, $body_open, $count);
        if ($body_close === null) {
            continue;
        }

        // Apply renames to parameter declarations and function body
        $fixes += apply_renames($tokens, $renames, $paren_open, $body_close);
    }

    if ($fixes === 0) {
        return 0;
    }

    // Rebuild content from tokens
    $new_content = '';
    for ($i = 0; $i < $count; $i++) {
        $new_content .= is_array($tokens[$i]) ? $tokens[$i][1] : $tokens[$i];
    }

    file_put_contents($filepath, $new_content);
    return $fixes;
}

/**
 * Find reserved keyword parameters in a parameter list.
 *
 * @return array Map of old_name => new_name for parameters that need renaming.
 */
function find_reserved_params($tokens, $paren_open, $paren_close) {
    $renames = [];
    $map = get_reserved_param_map();

    for ($i = $paren_open + 1; $i < $paren_close; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_VARIABLE) {
            continue;
        }

        $var_name = substr($tokens[$i][1], 1); // Strip $
        $lower = strtolower($var_name);

        if (isset($map[$lower])) {
            // Check if the new name would conflict with another parameter
            $new_name = $map[$lower];
            $renames['$' . $var_name] = '$' . $new_name;
        }
    }

    return $renames;
}

/**
 * Apply renames to tokens in a range (param list through function body end).
 *
 * @return int Number of parameters renamed.
 */
function apply_renames(&$tokens, $renames, $range_start, $range_end) {
    $fixes = 0;
    $param_fixed = [];

    for ($i = $range_start; $i <= $range_end; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_VARIABLE) {
            continue;
        }

        if (isset($renames[$tokens[$i][1]])) {
            $old = $tokens[$i][1];
            $tokens[$i][1] = $renames[$old];

            // Count each unique parameter rename once
            if (!isset($param_fixed[$old])) {
                $param_fixed[$old] = true;
                $fixes++;
            }
        }
    }

    return $fixes;
}

/**
 * Find next occurrence of a character token.
 */
function find_next_token($tokens, $start, $count, $char) {
    for ($i = $start; $i < $count; $i++) {
        if (!is_array($tokens[$i]) && $tokens[$i] === $char) {
            return $i;
        }
    }
    return null;
}

/**
 * Find matching closing paren.
 */
function find_matching_paren($tokens, $open, $count) {
    $depth = 1;
    for ($i = $open + 1; $i < $count; $i++) {
        $tok = is_array($tokens[$i]) ? null : $tokens[$i];
        if ($tok === '(') {
            $depth++;
        } elseif ($tok === ')') {
            $depth--;
            if ($depth === 0) {
                return $i;
            }
        }
    }
    return null;
}

/**
 * Find matching closing brace.
 */
function find_matching_brace($tokens, $open, $count) {
    $depth = 1;
    for ($i = $open + 1; $i < $count; $i++) {
        $tok = is_array($tokens[$i]) ? null : $tokens[$i];
        if ($tok === '{') {
            $depth++;
        } elseif ($tok === '}') {
            $depth--;
            if ($depth === 0) {
                return $i;
            }
        }
    }
    return null;
}
