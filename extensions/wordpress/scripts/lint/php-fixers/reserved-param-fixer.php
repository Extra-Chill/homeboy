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
 * Two-pass architecture for PHP 8 named argument safety:
 *   Pass 1: Scan all files, rename parameters in declarations + bodies,
 *           collect a manifest of all renames (method name → old param → new param)
 *   Pass 2: Scan all files again, find PHP 8 named argument call sites
 *           that use the old parameter name, update them to the new name
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

// Global declaration manifest used to classify which renames are safe.
$GLOBALS['declaration_manifest'] = build_declaration_manifest($path);

// Global manifest: safe renames only, used by both passes.
$GLOBALS['rename_manifest'] = build_safe_rename_manifest($GLOBALS['declaration_manifest']);

// Pass 1: Rename parameters in declarations + bodies, build manifest
$result = fixer_process_path($path, 'process_file_pass1');

// Pass 2: Update named argument call sites across all files
$callsite_fixes = 0;
if (!empty($GLOBALS['rename_manifest'])) {
    $callsite_result = fixer_process_path($path, 'process_file_pass2');
    $callsite_fixes = $callsite_result['total_fixes'];
}

$total = $result['total_fixes'] + $callsite_fixes;
if ($total > 0) {
    $parts = [];
    if ($result['total_fixes'] > 0) {
        $parts[] = "{$result['total_fixes']} parameter(s) in {$result['files_fixed']} file(s)";
    }
    if ($callsite_fixes > 0) {
        $parts[] = "$callsite_fixes named argument call site(s)";
    }
    echo "Reserved param fixer: Fixed " . implode(', ', $parts) . "\n";
} else {
    echo "Reserved param fixer: No fixable parameters found\n";
}

if (!empty($GLOBALS['reserved_param_stats']['ambiguous_declarations'])) {
    echo '  Skipped ambiguous declarations: ' . $GLOBALS['reserved_param_stats']['ambiguous_declarations'] . "\n";
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
 * Pass 1: Process a single PHP file — rename declarations + bodies, record manifest.
 */
function process_file_pass1($filepath) {
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

        // Get the function/method name
        $func_name = get_function_name($tokens, $i + 1, $count);

        // Found a function declaration. Find the parameter list.
        $paren_open = find_next_token($tokens, $i + 1, $count, '(');
        if ($paren_open === null) {
            continue;
        }

        $paren_close = find_matching_paren($tokens, $paren_open, $count);
        if ($paren_close === null) {
            continue;
        }

        // Extract parameter names from the parameter list.
        $safe_entry = ($func_name !== null && isset($GLOBALS['rename_manifest'][$func_name]))
            ? $GLOBALS['rename_manifest'][$func_name]
            : null;

        if ($safe_entry === null) {
            continue;
        }

        $renames = $safe_entry['variable_renames'];
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
 * Get token types that PHP's tokenizer uses for reserved keywords.
 *
 * When a reserved keyword like `class` appears as a named argument (class: value),
 * PHP's tokenizer does NOT produce T_STRING — it produces the keyword's own token
 * type (T_CLASS, T_DEFAULT, T_STATIC, etc.). We need to match all of these.
 *
 * @return array<int, bool> Map of token type IDs that can represent named arguments.
 */
function get_named_arg_token_types() {
    $types = [T_STRING => true]; // Always check T_STRING (parent, null, string, int, etc.)

    // Keywords that have their own token types
    $keyword_tokens = [
        T_DEFAULT, T_CLASS, T_LIST, T_ARRAY, T_CALLABLE, T_FN,
        T_SWITCH, T_RETURN, T_PRINT, T_ECHO, T_INCLUDE, T_REQUIRE,
        T_STATIC, T_FINAL, T_ABSTRACT, T_INTERFACE, T_TRAIT, T_GLOBAL, T_VAR,
    ];

    // T_MATCH exists in PHP 8.0+
    if (defined('T_MATCH')) {
        $keyword_tokens[] = T_MATCH;
    }
    // T_ENUM exists in PHP 8.1+
    if (defined('T_ENUM')) {
        $keyword_tokens[] = T_ENUM;
    }

    foreach ($keyword_tokens as $type) {
        $types[$type] = true;
    }

    return $types;
}

/**
 * Pass 2: Scan a file for PHP 8 named argument call sites and update them.
 *
 * Looks for patterns like `old_name:` that match renamed parameters in the manifest,
 * and rewrites them to `new_name:`.
 *
 * PHP 8 named arguments are tokenized differently depending on the keyword:
 *   - `class:` → T_CLASS ':'     (not T_STRING!)
 *   - `default:` → T_DEFAULT ':' (not T_STRING!)
 *   - `parent:` → T_STRING ':'   (T_STRING for some keywords)
 *
 * This function checks ALL relevant token types, not just T_STRING.
 */
function process_file_pass2($filepath) {
    $manifest = $GLOBALS['rename_manifest'];
    if (empty($manifest)) {
        return 0;
    }

    $content = file_get_contents($filepath);
    if ($content === false) {
        return 0;
    }

    // Quick pre-check: does this file contain any of the old named argument names?
    // This avoids tokenizing files that can't possibly have call sites.
    $has_candidate = false;
    foreach ($manifest as $method_name => $entry) {
        foreach ($entry['renames'] as $old_arg => $new_arg) {
            if (strpos($content, $old_arg . ':') !== false) {
                $has_candidate = true;
                break 2;
            }
        }
    }
    if (!$has_candidate) {
        return 0;
    }

    $tokens = @token_get_all($content);
    if ($tokens === false) {
        return 0;
    }

    // Build a flat lookup: old_arg_name => new_arg_name (across all methods)
    // Also track which method names each arg belongs to for context checking
    $arg_to_methods = []; // old_arg => [method_name, ...]
    $arg_renames = [];    // old_arg => new_arg
    foreach ($manifest as $method_name => $entry) {
        foreach ($entry['renames'] as $old_arg => $new_arg) {
            $arg_renames[$old_arg] = $new_arg;
            $arg_to_methods[$old_arg][] = $method_name;
        }
    }

    $named_arg_types = get_named_arg_token_types();
    $count = count($tokens);
    $fixes = 0;

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i])) {
            continue;
        }

        // Check if this token type can represent a named argument
        if (!isset($named_arg_types[$tokens[$i][0]])) {
            continue;
        }

        $token_text = $tokens[$i][1];
        if (!isset($arg_renames[$token_text])) {
            continue;
        }

        // Check if this token is followed by ':' (named argument syntax)
        // Skip whitespace between the token and ':'
        $colon_idx = find_next_non_whitespace($tokens, $i + 1, $count);
        if ($colon_idx === null) {
            continue;
        }

        // The colon must be a plain ':' character, not '::' (scope resolution)
        if (is_array($tokens[$colon_idx]) || $tokens[$colon_idx] !== ':') {
            continue;
        }

        // Verify '::' is not what we're looking at (T_DOUBLE_COLON is a separate token,
        // but be extra safe)
        if ($colon_idx + 1 < $count && !is_array($tokens[$colon_idx + 1]) && $tokens[$colon_idx + 1] === ':') {
            continue;
        }

        // Make sure we're inside a function call (inside parentheses)
        // and this looks like a named argument context.
        // Verify we're not in a ternary, switch case, goto label, etc.
        if (!is_named_argument_context($tokens, $i, $count)) {
            continue;
        }

        // Check if a relevant method name appears nearby (within reasonable distance)
        // This prevents false positives on unrelated named arguments
        if (!has_method_call_context($tokens, $i, $arg_to_methods[$token_text])) {
            continue;
        }

        // Apply the rename — change the token to T_STRING with the new name
        $tokens[$i] = [T_STRING, $arg_renames[$token_text], $tokens[$i][2]];
        $fixes++;
    }

    if ($fixes === 0) {
        return 0;
    }

    // Rebuild and write
    $new_content = '';
    for ($i = 0; $i < $count; $i++) {
        $new_content .= is_array($tokens[$i]) ? $tokens[$i][1] : $tokens[$i];
    }

    file_put_contents($filepath, $new_content);
    return $fixes;
}

/**
 * Get the function/method name from tokens after T_FUNCTION.
 *
 * @return string|null The function name, or null for closures.
 */
function get_function_name($tokens, $start, $count) {
    for ($i = $start; $i < $count; $i++) {
        if (!is_array($tokens[$i])) {
            // Hit '(' before finding a name — this is a closure
            if ($tokens[$i] === '(') {
                return null;
            }
            continue;
        }

        if ($tokens[$i][0] === T_WHITESPACE) {
            continue;
        }

        if ($tokens[$i][0] === T_STRING) {
            return $tokens[$i][1];
        }

        // Unexpected token
        return null;
    }
    return null;
}

/**
 * Build a declaration inventory for all named functions and methods.
 *
 * @param string $path Path being processed.
 * @return array<string, array<int, array<string, mixed>>>
 */
function build_declaration_manifest($path) {
    $manifest = [];

    fixer_process_path($path, function ($filepath) use (&$manifest) {
        $content = file_get_contents($filepath);
        if ($content === false) {
            return 0;
        }

        $tokens = @token_get_all($content);
        if ($tokens === false) {
            return 0;
        }

        $count = count($tokens);
        for ($i = 0; $i < $count; $i++) {
            if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_FUNCTION) {
                continue;
            }

            $func_name = get_function_name($tokens, $i + 1, $count);
            if ($func_name === null) {
                continue;
            }

            $paren_open = find_next_token($tokens, $i + 1, $count, '(');
            if ($paren_open === null) {
                continue;
            }

            $paren_close = find_matching_paren($tokens, $paren_open, $count);
            if ($paren_close === null) {
                continue;
            }

            $variable_renames = find_reserved_params($tokens, $paren_open, $paren_close);
            $renames = normalize_named_arg_renames($variable_renames);
            $manifest[$func_name][] = [
                'identity'         => get_declaration_identity($tokens, $i, $func_name),
                'file'             => $filepath,
                'line'             => $tokens[$i][2],
                'variable_renames' => $variable_renames,
                'renames'          => $renames,
            ];
        }

        return 0;
    });

    return $manifest;
}

/**
 * Keep only renames whose callable name is semantically safe to rewrite.
 *
 * Safe means every declaration sharing that callable name exposes the exact same
 * reserved-param rename map. Anything else is ambiguous, so we report-only.
 *
 * @param array<string, array<int, array<string, mixed>>> $declaration_manifest
 * @return array<string, array<string, mixed>>
 */
function build_safe_rename_manifest($declaration_manifest) {
    $safe_manifest = [];
    $stats = [
        'ambiguous_callables'    => 0,
        'ambiguous_declarations' => 0,
    ];

    foreach ($declaration_manifest as $func_name => $entries) {
        $fingerprints = [];
        $has_renames  = false;

        foreach ($entries as $entry) {
            if (!empty($entry['renames'])) {
                $has_renames = true;
            }

            $fingerprints[json_encode($entry['renames'])] = true;
        }

        if (!$has_renames) {
            continue;
        }

        if (count($fingerprints) !== 1) {
            $stats['ambiguous_callables']++;
            foreach ($entries as $entry) {
                if (!empty($entry['renames'])) {
                    $stats['ambiguous_declarations']++;
                }
            }
            continue;
        }

        $safe_manifest[$func_name] = [
            'renames'          => $entries[0]['renames'],
            'variable_renames' => $entries[0]['variable_renames'],
            'declarations'     => $entries,
        ];
    }

    $GLOBALS['reserved_param_stats'] = $stats;

    return $safe_manifest;
}

/**
 * Convert variable rename map to named-argument rename map and sort it.
 *
 * @param array<string, string> $renames Variable renames with leading $.
 * @return array<string, string>
 */
function normalize_named_arg_renames($renames) {
    $named_arg_renames = [];

    foreach ($renames as $old_var => $new_var) {
        $named_arg_renames[substr($old_var, 1)] = substr($new_var, 1);
    }

    ksort($named_arg_renames);

    return $named_arg_renames;
}

/**
 * Build a readable declaration identity for debugging/reporting.
 *
 * @param array       $tokens Token stream.
 * @param int         $func_token_index Function token index.
 * @param string      $func_name Function name.
 * @return string
 */
function get_declaration_identity($tokens, $func_token_index, $func_name) {
    $scope_name = find_enclosing_scope_name($tokens, $func_token_index);

    if ($scope_name === null) {
        return $func_name;
    }

    return $scope_name . '::' . $func_name;
}

/**
 * Find the enclosing class/trait/interface/enum name for a function token.
 *
 * @param array $tokens Token stream.
 * @param int   $position Token index.
 * @return string|null
 */
function find_enclosing_scope_name($tokens, $position) {
    $count = count($tokens);
    $scope_stack = [];
    $brace_depth = 0;
    $scope_tokens = [T_CLASS, T_INTERFACE, T_TRAIT];

    if (defined('T_ENUM')) {
        $scope_tokens[] = T_ENUM;
    }

    for ($i = 0; $i < $count; $i++) {
        if (is_array($tokens[$i]) && in_array($tokens[$i][0], $scope_tokens, true)) {
            for ($j = $i + 1; $j < $count; $j++) {
                if (is_array($tokens[$j]) && $tokens[$j][0] === T_STRING) {
                    $scope_stack[] = [
                        'name'  => $tokens[$j][1],
                        'depth' => $brace_depth,
                    ];
                    break;
                }

                if (!is_array($tokens[$j]) && $tokens[$j] === '{') {
                    break;
                }
            }
        } elseif (!is_array($tokens[$i])) {
            if ($tokens[$i] === '{') {
                $brace_depth++;
            } elseif ($tokens[$i] === '}') {
                $brace_depth--;
                while (!empty($scope_stack) && end($scope_stack)['depth'] >= $brace_depth) {
                    array_pop($scope_stack);
                }
            }
        }

        if ($i === $position) {
            return empty($scope_stack) ? null : end($scope_stack)['name'];
        }
    }

    return null;
}

/**
 * Check if a T_STRING at position $idx is in a named argument context.
 *
 * Named arguments only appear inside function/method call parentheses.
 * We check that:
 *   1. We're inside parentheses (paren depth > 0)
 *   2. The T_STRING is preceded by '(' or ',' (possibly with whitespace)
 *      — i.e., it's at the start of an argument position
 */
function is_named_argument_context($tokens, $idx, $count) {
    // Check paren depth at this position — must be inside parens
    $depth = 0;
    for ($i = 0; $i < $idx; $i++) {
        if (!is_array($tokens[$i])) {
            if ($tokens[$i] === '(') {
                $depth++;
            } elseif ($tokens[$i] === ')') {
                $depth--;
            }
        }
    }
    if ($depth <= 0) {
        return false;
    }

    // Check that the previous non-whitespace token is '(' or ','
    // This ensures we're at the start of an argument slot
    $prev = find_prev_non_whitespace($tokens, $idx - 1);
    if ($prev === null) {
        return false;
    }

    if (is_array($tokens[$prev])) {
        return false; // Previous token is a keyword/identifier, not a delimiter
    }

    return ($tokens[$prev] === '(' || $tokens[$prev] === ',');
}

/**
 * Check if any of the target method names appear in the call chain
 * leading to this named argument position.
 *
 * Walks backward from the named argument to find the function call name.
 */
function has_method_call_context($tokens, $arg_idx, $method_names) {
    // Walk backward to find the opening '(' of the call we're inside
    $depth = 0;
    $call_paren = null;
    for ($i = $arg_idx - 1; $i >= 0; $i--) {
        if (!is_array($tokens[$i])) {
            if ($tokens[$i] === ')') {
                $depth++;
            } elseif ($tokens[$i] === '(') {
                if ($depth === 0) {
                    $call_paren = $i;
                    break;
                }
                $depth--;
            }
        }
    }

    if ($call_paren === null) {
        return false;
    }

    // The token before '(' should be the function/method name
    $name_idx = find_prev_non_whitespace($tokens, $call_paren - 1);
    if ($name_idx === null) {
        return false;
    }

    if (!is_array($tokens[$name_idx]) || $tokens[$name_idx][0] !== T_STRING) {
        return false;
    }

    $call_name = $tokens[$name_idx][1];
    return in_array($call_name, $method_names, true);
}

/**
 * Find the next non-whitespace token index.
 */
function find_next_non_whitespace($tokens, $start, $count) {
    for ($i = $start; $i < $count; $i++) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        return $i;
    }
    return null;
}

/**
 * Find the previous non-whitespace token index.
 */
function find_prev_non_whitespace($tokens, $start) {
    for ($i = $start; $i >= 0; $i--) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        return $i;
    }
    return null;
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
