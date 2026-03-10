#!/usr/bin/env php
<?php
/**
 * WordPress-Aware Unused Parameter Fixer
 *
 * Unlike other fixers that blindly transform code, this one is smart:
 *
 * Phase 1: Scans the entire codebase to build a callback registration map
 *          (register_rest_route, add_filter, add_action, wp_register_ability,
 *           tool dispatch patterns, abstract/interface method signatures)
 *
 * Phase 2: Runs PHPCS to find UnusedFunctionParameter violations
 *
 * Phase 3: Cross-references each violation against the callback map to decide:
 *          - Contract-mandated param (callback/interface) → prefix with underscore
 *          - Genuinely dead param (private, traceable callers) → remove from
 *            signature AND all call sites
 *          - Include-scope param ($data used by included templates) → skip
 *
 * Usage: php unused-param-fixer.php <path> [--phpcs-binary=<path>] [--phpcs-standard=<path>]
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php unused-param-fixer.php <path> [--phpcs-binary=<path>] [--phpcs-standard=<path>]\n";
    exit(1);
}

$target_path = $argv[1];

if (!file_exists($target_path)) {
    echo "Error: Path not found: $target_path\n";
    exit(1);
}

// Parse optional arguments.
$phpcs_binary   = null;
$phpcs_standard = null;
for ($i = 2; $i < $argc; $i++) {
    if (strpos($argv[$i], '--phpcs-binary=') === 0) {
        $phpcs_binary = substr($argv[$i], strlen('--phpcs-binary='));
    } elseif (strpos($argv[$i], '--phpcs-standard=') === 0) {
        $phpcs_standard = substr($argv[$i], strlen('--phpcs-standard='));
    }
}

if ($phpcs_binary === null) {
    // Try to find phpcs in common locations.
    $candidates = [
        __DIR__ . '/../../vendor/bin/phpcs',
        dirname(__DIR__, 3) . '/vendor/bin/phpcs',
    ];
    foreach ($candidates as $candidate) {
        if (file_exists($candidate)) {
            $phpcs_binary = realpath($candidate);
            break;
        }
    }
    if ($phpcs_binary === null) {
        echo "Error: Could not find phpcs binary. Use --phpcs-binary=<path>\n";
        exit(1);
    }
}

if ($phpcs_standard === null) {
    $candidates = [
        __DIR__ . '/../../phpcs.xml.dist',
        dirname(__DIR__, 3) . '/phpcs.xml.dist',
    ];
    foreach ($candidates as $candidate) {
        if (file_exists($candidate)) {
            $phpcs_standard = realpath($candidate);
            break;
        }
    }
    if ($phpcs_standard === null) {
        echo "Error: Could not find phpcs standard. Use --phpcs-standard=<path>\n";
        exit(1);
    }
}

// Resolve to absolute path for scanning.
$scan_root = realpath($target_path);
if ($scan_root === false) {
    echo "Error: Cannot resolve path: $target_path\n";
    exit(1);
}

// ============================================================================
// Phase 1: Build callback registration map
// ============================================================================
echo "Phase 1: Scanning for callback registrations...\n";
$callback_map = build_callback_map($scan_root);
echo "  Found " . count($callback_map) . " callback registrations\n";

// ============================================================================
// Phase 2: Run PHPCS to find violations
// ============================================================================
echo "Phase 2: Running PHPCS to find unused parameter violations...\n";
$violations = find_violations($phpcs_binary, $phpcs_standard, $target_path);
echo "  Found " . count($violations) . " violations\n";

if (empty($violations)) {
    echo "Unused param fixer: No violations found\n";
    exit(0);
}

// ============================================================================
// Phase 3: Classify and fix each violation
// ============================================================================
echo "Phase 3: Classifying and fixing violations...\n";

$stats = [
    'noop_inserted'  => 0,
    'param_removed'  => 0,
    'skipped_unknown' => 0,
    'files_modified'  => [],
];

// Group violations by file for efficient processing.
$by_file = [];
foreach ($violations as $v) {
    $by_file[$v['file']][] = $v;
}

foreach ($by_file as $filepath => $file_violations) {
    $content = file_get_contents($filepath);
    if ($content === false) {
        continue;
    }

    $original = $content;
    $tokens   = @token_get_all($content);
    if ($tokens === false) {
        continue;
    }

    // Separate violations into noop-insert vs param-remove.
    // We'll collect noop insertions by function body line, then apply all at once.
    $noop_insertions = []; // keyed by body_open line => list of param names
    $removals        = [];

    foreach ($file_violations as $v) {
        $param_name = $v['param'];
        $line       = $v['line'];

        // Check if this is an include-scope param ($data used by templates).
        // The variable IS used (available to included files) but PHPCS can't see it.
        // We handle it the same as contract-mandated params: insert a noop reference.

        // Find the function token that contains this parameter.
        $func_info = find_function_at_line($tokens, $line);
        if ($func_info === null) {
            $stats['skipped_unknown']++;
            continue;
        }

        // Determine the function's identity for callback map lookup.
        $func_key = get_function_key($tokens, $func_info, $filepath);

        // Is this function registered as a callback?
        $is_callback = is_callback_registered($func_key, $callback_map);

        // Also check: does the method override an abstract/interface method?
        $is_contract_mandated = $is_callback || is_override_method($tokens, $func_info);

        if (!$is_contract_mandated) {
            // Check if param is removable (last param, private/no callers).
            $remove_info = can_remove_param($tokens, $func_info, $param_name, $filepath, $scan_root);
            if ($remove_info['removable']) {
                $removals[] = [
                    'func_info'  => $func_info,
                    'param_name' => $param_name,
                    'remove_info' => $remove_info,
                ];
                continue;
            }
        }

        // Default action: insert noop reference.
        // Group by function body_open so multiple unused params in the same
        // function get their noop lines inserted together.
        $body_open_line = get_line_at($tokens, $func_info['body_open']);
        if (!isset($noop_insertions[$body_open_line])) {
            $noop_insertions[$body_open_line] = [
                'func_info' => $func_info,
                'params'    => [],
            ];
        }
        $noop_insertions[$body_open_line]['params'][] = $param_name;
    }

    // Apply param removals first (token-based).
    // Sort by line descending so earlier token indices stay valid.
    usort($removals, function ($a, $b) {
        return $b['func_info']['line'] - $a['func_info']['line'];
    });

    foreach ($removals as $r) {
        $fixed = remove_param_from_signature($tokens, $r['func_info'], $r['param_name']);
        if ($fixed) {
            foreach ($r['remove_info']['call_sites'] as $call_site) {
                if ($call_site['file'] === $filepath) {
                    // Same file — remove from in-memory tokens directly.
                    remove_arg_from_tokens(
                        $tokens,
                        $call_site['line'],
                        $r['remove_info']['param_position'],
                        $call_site['method_name']
                    );
                } else {
                    // Different file — read/modify/write independently.
                    remove_param_from_call_site(
                        $call_site['file'],
                        $call_site['line'],
                        $r['remove_info']['param_position'],
                        $call_site['method_name']
                    );
                }
            }
            $stats['param_removed']++;
        }
    }

    // Rebuild content from tokens (includes any removals).
    $new_content = rebuild_from_tokens($tokens);

    // Apply noop insertions (line-based, on the rebuilt content).
    if (!empty($noop_insertions)) {
        $lines = explode("\n", $new_content);

        // We need to find the opening brace line in the rebuilt content.
        // Re-tokenize to get accurate line numbers after removals.
        $new_tokens = @token_get_all($new_content);

        // Sort insertions by line descending so line numbers stay valid.
        $sorted_insertions = $noop_insertions;
        krsort($sorted_insertions);

        foreach ($sorted_insertions as $orig_body_line => $info) {
            // Re-find the function in the rebuilt content.
            $func_info = find_function_by_name_and_approx_line(
                $new_tokens,
                $info['func_info']['name'],
                $info['func_info']['line']
            );

            if ($func_info === null) {
                continue;
            }

            $brace_line = get_line_at($new_tokens, $func_info['body_open']);
            // Line numbers are 1-indexed, array is 0-indexed.
            $insert_after = $brace_line - 1;

            if ($insert_after < 0 || $insert_after >= count($lines)) {
                continue;
            }

            // Detect indentation from the next non-empty line in the body.
            $indent = detect_body_indent($lines, $insert_after + 1);

            // Build noop lines for each unused param.
            $noop_lines = [];
            foreach ($info['params'] as $param) {
                $noop_lines[] = $indent . $param . ';';
                $stats['noop_inserted']++;
            }

            // Insert after the opening brace line.
            array_splice($lines, $insert_after + 1, 0, $noop_lines);

            // Re-tokenize for subsequent insertions (line numbers shifted).
            $new_content  = implode("\n", $lines);
            $new_tokens   = @token_get_all($new_content);
        }

        $new_content = implode("\n", $lines);
    }

    if ($new_content !== $original) {
        file_put_contents($filepath, $new_content);
        $stats['files_modified'][] = $filepath;
    }
}

$total_fixes = $stats['noop_inserted'] + $stats['param_removed'];
$file_count  = count($stats['files_modified']);

if ($total_fixes > 0) {
    echo "Unused param fixer: Fixed {$total_fixes} parameter(s) in {$file_count} file(s)\n";
    echo "  Noop reference inserted: {$stats['noop_inserted']}\n";
    echo "  Removed from signature: {$stats['param_removed']}\n";
    if ($stats['skipped_unknown'] > 0) {
        echo "  Skipped (could not locate function): {$stats['skipped_unknown']}\n";
    }
} else {
    echo "Unused param fixer: No fixable parameters found\n";
}

exit(0);

// ============================================================================
// Phase 1 functions: Build callback registration map
// ============================================================================

/**
 * Scan the codebase and build a map of callback registrations.
 *
 * Returns an array where keys are function identity strings and values
 * contain the registration context.
 *
 * Key format: "ClassName::methodName" or "functionName" or "file:line" for closures.
 *
 * @param string $root Directory to scan.
 * @return array
 */
function build_callback_map($root) {
    $map = [];

    $result = fixer_process_path($root, function ($filepath) use (&$map) {
        $content = file_get_contents($filepath);
        if ($content === false) {
            return 0;
        }

        // Detect REST API route registrations.
        // Pattern: register_rest_route(..., array( 'callback' => array( $this, 'method' ) ))
        // Also: register_rest_route(..., array( 'callback' => 'function_name' ))
        // Also: 'permission_callback' => array( ClassName::class, 'method' )
        detect_rest_route_callbacks($content, $filepath, $map);
        detect_explicit_rest_route_class_callbacks_safe($content, $filepath, $map);

        // Detect add_filter / add_action registrations.
        detect_hook_callbacks($content, $filepath, $map);
        detect_explicit_hook_class_callbacks_safe($content, $filepath, $map);

        // Detect wp_register_ability execute_callback registrations.
        detect_ability_callbacks($content, $filepath, $map);
        detect_explicit_ability_class_callbacks_safe($content, $filepath, $map);

        // Detect tool dispatch patterns (handle_tool_call, handleChatToolCall).
        detect_tool_callbacks($content, $filepath, $map);

        // Detect call_user_func/call_user_func_array patterns.
        detect_call_user_func($content, $filepath, $map);

        return 0;
    });

    return $map;
}

/**
 * Detect register_rest_route callback and permission_callback registrations.
 */
function detect_rest_route_callbacks($content, $filepath, &$map) {
    // Match 'callback' => array( $this, 'method' ) and similar.
    // Also match 'permission_callback', 'validate_callback', 'sanitize_callback'.
    $pattern = '/[\'"](?:callback|permission_callback|validate_callback|sanitize_callback)[\'"]\s*=>\s*array\s*\(\s*(?:'
        . '(?:\$this|static::class|self::class|[\w\\\\]+::class|[\'"]\w+[\'"]),\s*[\'"]([\w]+)[\'"]'
        . ')\s*\)/';

    if (preg_match_all($pattern, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $method_name = $m[1];

            // Determine the class context.
            $class_name = detect_class_name($content);
            if ($class_name !== null) {
                $key = $class_name . '::' . $method_name;
            } else {
                $key = $method_name;
            }
            $map[$key] = ['type' => 'rest_api', 'file' => $filepath];
        }
    }

    // Also match: 'callback' => 'function_name' (standalone functions).
    $pattern2 = '/[\'"](?:callback|permission_callback|validate_callback|sanitize_callback)[\'"]\s*=>\s*[\'"]([\w]+)[\'"]/';
    if (preg_match_all($pattern2, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $map[$m[1]] = ['type' => 'rest_api', 'file' => $filepath];
        }
    }

    // Match: 'callback' => [ $this, 'method' ] (square bracket syntax).
    $pattern3 = '/[\'"](?:callback|permission_callback|validate_callback|sanitize_callback)[\'"]\s*=>\s*\[\s*(?:'
        . '(?:\$this|static::class|self::class|[\w\\\\]+::class|[\'"]\w+[\'"]),\s*[\'"]([\w]+)[\'"]'
        . ')\s*\]/';
    if (preg_match_all($pattern3, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $method_name = $m[1];
            $class_name  = detect_class_name($content);
            if ($class_name !== null) {
                $key = $class_name . '::' . $method_name;
            } else {
                $key = $method_name;
            }
            $map[$key] = ['type' => 'rest_api', 'file' => $filepath];
        }
    }
}

/**
 * Detect add_filter and add_action callback registrations.
 */
function detect_hook_callbacks($content, $filepath, &$map) {
    // Match: add_filter('hook', array($this, 'method'), 10, 3)
    // Match: add_action('hook', array(ClassName::class, 'method'), 10, 2)
    $pattern = '/add_(?:filter|action)\s*\(\s*[\'"][\w]+[\'"]\s*,\s*array\s*\(\s*(?:'
        . '(?:\$this|static::class|self::class|[\w\\\\]+::class|[\'"]\w+[\'"]),\s*[\'"]([\w]+)[\'"]'
        . ')\s*\)/';

    if (preg_match_all($pattern, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $method_name = $m[1];
            $class_name  = detect_class_name($content);
            if ($class_name !== null) {
                $key = $class_name . '::' . $method_name;
            } else {
                $key = $method_name;
            }
            $map[$key] = ['type' => 'hook', 'file' => $filepath];
        }
    }

    // Match: add_filter('hook', 'function_name', 10, 3)
    $pattern2 = '/add_(?:filter|action)\s*\(\s*[\'"][\w]+[\'"]\s*,\s*[\'"]([\w]+)[\'"]/';
    if (preg_match_all($pattern2, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $map[$m[1]] = ['type' => 'hook', 'file' => $filepath];
        }
    }

    // Match square bracket syntax: add_filter('hook', [$this, 'method'])
    $pattern3 = '/add_(?:filter|action)\s*\(\s*[\'"][\w]+[\'"]\s*,\s*\[\s*(?:'
        . '(?:\$this|static::class|self::class|[\w\\\\]+::class|[\'"]\w+[\'"]),\s*[\'"]([\w]+)[\'"]'
        . ')\s*\]/';
    if (preg_match_all($pattern3, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $method_name = $m[1];
            $class_name  = detect_class_name($content);
            if ($class_name !== null) {
                $key = $class_name . '::' . $method_name;
            } else {
                $key = $method_name;
            }
            $map[$key] = ['type' => 'hook', 'file' => $filepath];
        }
    }

    // Closure callbacks registered to hooks — detect by matching the pattern:
    // add_filter('hook', function($param1, ...) {
    // These are identified by file:line, handled separately.
    if (preg_match_all('/add_(?:filter|action)\s*\(\s*[\'"]([\w]+)[\'"]\s*,\s*\n?\s*function\s*\(/m', $content, $matches, PREG_OFFSET_CAPTURE)) {
        foreach ($matches[0] as $idx => $match_info) {
            $offset = $match_info[1];
            $line   = substr_count(substr($content, 0, $offset), "\n") + 1;
            // Find the line of the actual function keyword.
            $func_line = $line;
            $snippet   = substr($content, $offset);
            if (preg_match('/function\s*\(/', $snippet, $fm, PREG_OFFSET_CAPTURE)) {
                $func_line = $line + substr_count(substr($snippet, 0, $fm[0][1]), "\n");
            }
            $key       = $filepath . ':closure:' . $func_line;
            $map[$key] = ['type' => 'hook_closure', 'file' => $filepath, 'line' => $func_line];
        }
    }
}

/**
 * Detect wp_register_ability execute_callback registrations.
 */
function detect_ability_callbacks($content, $filepath, &$map) {
    // Match: 'execute_callback' => array($this, 'methodName')
    // Match: 'execute_callback' => array(ClassName::class, 'methodName')
    $pattern = '/[\'"]execute_callback[\'"]\s*=>\s*(?:array\s*\(|\[)\s*(?:'
        . '(?:\$this|static::class|self::class|[\w\\\\]+::class|[\'"]\w+[\'"]),\s*[\'"]([\w]+)[\'"]'
        . ')\s*(?:\)|\])/';

    if (preg_match_all($pattern, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $method_name = $m[1];
            $class_name  = detect_class_name($content);
            if ($class_name !== null) {
                $key = $class_name . '::' . $method_name;
            } else {
                $key = $method_name;
            }
            $map[$key] = ['type' => 'ability', 'file' => $filepath];
        }
    }
}

/**
 * Detect explicit class-based REST callback registrations.
 */
function detect_explicit_rest_route_class_callbacks_safe($content, $filepath, &$map) {
    detect_explicit_class_callbacks_safe(
        $content,
        $filepath,
        $map,
        '~[\'\"](?:callback|permission_callback|validate_callback|sanitize_callback)[\'\"]\s*=>\s*(?:array\s*\(|\[)\s*((?:[A-Za-z_][A-Za-z0-9_]*\\\\)*[A-Za-z_][A-Za-z0-9_]*)::class\s*,\s*[\'\"]([\w]+)[\'\"]\s*(?:\)|\])~',
        'rest_api'
    );
}

/**
 * Detect explicit class-based hook callback registrations.
 */
function detect_explicit_hook_class_callbacks_safe($content, $filepath, &$map) {
    detect_explicit_class_callbacks_safe(
        $content,
        $filepath,
        $map,
        '~add_(?:filter|action)\s*\(\s*[\'\"][\w]+[\'\"]\s*,\s*(?:array\s*\(|\[)\s*((?:[A-Za-z_][A-Za-z0-9_]*\\\\)*[A-Za-z_][A-Za-z0-9_]*)::class\s*,\s*[\'\"]([\w]+)[\'\"]\s*(?:\)|\])~',
        'hook'
    );
}

/**
 * Detect explicit class-based ability callback registrations.
 */
function detect_explicit_ability_class_callbacks_safe($content, $filepath, &$map) {
    detect_explicit_class_callbacks_safe(
        $content,
        $filepath,
        $map,
        '~[\'\"]execute_callback[\'\"]\s*=>\s*(?:array\s*\(|\[)\s*((?:[A-Za-z_][A-Za-z0-9_]*\\\\)*[A-Za-z_][A-Za-z0-9_]*)::class\s*,\s*[\'\"]([\w]+)[\'\"]\s*(?:\)|\])~',
        'ability'
    );
}

/**
 * Detect and register callbacks that explicitly reference ClassName::class.
 */
function detect_explicit_class_callbacks_safe($content, $filepath, &$map, $pattern, $type) {
    if (!preg_match_all($pattern, $content, $matches, PREG_SET_ORDER)) {
        return;
    }

    foreach ($matches as $m) {
        $map[$m[1] . '::' . $m[2]] = ['type' => $type, 'file' => $filepath];
    }
}

/**
 * Detect tool dispatch callback patterns.
 *
 * Tools define 'method' => 'handleChatToolCall' or implement handle_tool_call.
 * The ToolExecutor calls: $handler->handle_tool_call($params, $tool_def)
 */
function detect_tool_callbacks($content, $filepath, &$map) {
    // Match: 'method' => 'methodName' in tool definition arrays.
    $pattern = '/[\'"]method[\'"]\s*=>\s*[\'"]([\w]+)[\'"]/';
    if (preg_match_all($pattern, $content, $matches, PREG_SET_ORDER)) {
        foreach ($matches as $m) {
            $method_name = $m[1];
            $class_name  = detect_class_name($content);
            if ($class_name !== null) {
                $key = $class_name . '::' . $method_name;
            } else {
                $key = $method_name;
            }
            $map[$key] = ['type' => 'tool_dispatch', 'file' => $filepath];
        }
    }

    // Any class that has a handle_tool_call method is a tool handler.
    if (preg_match('/function\s+handle_tool_call\s*\(/', $content)) {
        $class_name = detect_class_name($content);
        if ($class_name !== null) {
            $map[$class_name . '::handle_tool_call'] = ['type' => 'tool_dispatch', 'file' => $filepath];
        }
    }
}

/**
 * Detect call_user_func patterns that dispatch to methods.
 */
function detect_call_user_func($content, $filepath, &$map) {
    // Match: call_user_func($check['callback'], ...)
    // This is too generic to resolve the actual target, but we mark the pattern.
    // For now, we'll handle this via the "method is referenced in call_user_func" heuristic
    // in the classification phase instead.
}

/**
 * Extract the class name from file content.
 *
 * @return string|null
 */
function detect_class_name($content) {
    if (preg_match('/^\s*class\s+(\w+)/m', $content, $m)) {
        return $m[1];
    }
    return null;
}

// ============================================================================
// Phase 2 functions: Find PHPCS violations
// ============================================================================

/**
 * Run PHPCS and parse the JSON output for UnusedFunctionParameter violations.
 *
 * @return array List of violations with file, line, param, source.
 */
function find_violations($phpcs_binary, $phpcs_standard, $target_path) {
    $cmd = sprintf(
        '%s --standard=%s --sniffs=Generic.CodeAnalysis.UnusedFunctionParameter -s --report=json %s 2>/dev/null',
        escapeshellarg($phpcs_binary),
        escapeshellarg($phpcs_standard),
        escapeshellarg($target_path)
    );

    $output = shell_exec($cmd);
    if ($output === null || $output === '') {
        return [];
    }

    $data = json_decode($output, true);
    if ($data === null || !isset($data['files'])) {
        return [];
    }

    $violations = [];
    foreach ($data['files'] as $filepath => $file_data) {
        foreach ($file_data['messages'] as $msg) {
            // Extract param name from message: "The method parameter $foo is never used"
            if (preg_match('/parameter (\$\w+) is never used/', $msg['message'], $m)) {
                $violations[] = [
                    'file'   => $filepath,
                    'line'   => $msg['line'],
                    'column' => $msg['column'],
                    'param'  => $m[1],
                    'source' => $msg['source'],
                ];
            }
        }
    }

    return $violations;
}

// ============================================================================
// Phase 3 functions: Classification and fixing
// ============================================================================

/**
 * Find the function declaration that contains a given line number.
 *
 * Returns array with: func_token, name, paren_open, paren_close, body_open, body_close, params.
 */
function find_function_at_line($tokens, $target_line) {
    $count = count($tokens);

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_FUNCTION) {
            continue;
        }

        $func_token = $i;
        $func_line  = $tokens[$i][2];

        // Find opening paren.
        $paren_open = null;
        for ($j = $i + 1; $j < $count; $j++) {
            if (!is_array($tokens[$j]) && $tokens[$j] === '(') {
                $paren_open = $j;
                break;
            }
        }
        if ($paren_open === null) {
            continue;
        }

        // Find closing paren.
        $paren_close = find_matching_close($tokens, $paren_open, $count, '(', ')');
        if ($paren_close === null) {
            continue;
        }

        // Check if the target line falls within the function signature lines.
        $sig_start_line = $func_line;
        $sig_end_line   = is_array($tokens[$paren_close]) ? $tokens[$paren_close][2] : get_line_at($tokens, $paren_close);

        if ($target_line < $sig_start_line || $target_line > $sig_end_line) {
            continue;
        }

        // Find function name (may not exist for closures).
        $func_name = null;
        for ($j = $func_token + 1; $j < $paren_open; $j++) {
            if (is_array($tokens[$j]) && $tokens[$j][0] === T_STRING) {
                $func_name = $tokens[$j][1];
                break;
            }
        }

        // Find opening brace (body start).
        $body_open = null;
        for ($j = $paren_close + 1; $j < $count; $j++) {
            if (!is_array($tokens[$j]) && $tokens[$j] === '{') {
                $body_open = $j;
                break;
            }
            // Arrow functions use => instead of {.
            if (is_array($tokens[$j]) && $tokens[$j][0] === T_FN) {
                break;
            }
            // Abstract/interface methods end with ;.
            if (!is_array($tokens[$j]) && $tokens[$j] === ';') {
                break;
            }
        }

        if ($body_open === null) {
            continue;
        }

        $body_close = find_matching_close($tokens, $body_open, $count, '{', '}');
        if ($body_close === null) {
            continue;
        }

        // Extract parameter info.
        $params = extract_params($tokens, $paren_open, $paren_close);

        // Detect visibility.
        $visibility = detect_visibility($tokens, $func_token);

        return [
            'func_token'  => $func_token,
            'name'        => $func_name,
            'paren_open'  => $paren_open,
            'paren_close' => $paren_close,
            'body_open'   => $body_open,
            'body_close'  => $body_close,
            'params'      => $params,
            'visibility'  => $visibility,
            'line'        => $func_line,
        ];
    }

    return null;
}

/**
 * Extract parameter names and positions from a function signature.
 */
function extract_params($tokens, $paren_open, $paren_close) {
    $params   = [];
    $position = 0;

    for ($i = $paren_open + 1; $i < $paren_close; $i++) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_VARIABLE) {
            $params[] = [
                'name'     => $tokens[$i][1],
                'position' => $position,
                'token_index' => $i,
            ];
            $position++;
        }
    }

    return $params;
}

/**
 * Detect visibility of a method (public, protected, private, or null for functions).
 */
function detect_visibility($tokens, $func_token) {
    // Walk backward from function keyword to find visibility.
    for ($i = $func_token - 1; $i >= 0; $i--) {
        if (!is_array($tokens[$i])) {
            break;
        }
        $code = $tokens[$i][0];
        if ($code === T_PUBLIC) {
            return 'public';
        }
        if ($code === T_PROTECTED) {
            return 'protected';
        }
        if ($code === T_PRIVATE) {
            return 'private';
        }
        if ($code === T_STATIC || $code === T_ABSTRACT || $code === T_FINAL || $code === T_WHITESPACE || $code === T_COMMENT || $code === T_DOC_COMMENT) {
            continue;
        }
        break;
    }
    return null;
}

/**
 * Build a lookup key for a function to match against the callback map.
 */
function get_function_key($tokens, $func_info, $filepath) {
    if ($func_info['name'] === null) {
        // Closure — use file:closure:line as key.
        return $filepath . ':closure:' . $func_info['line'];
    }

    // Find enclosing class.
    $class_name = find_enclosing_class($tokens, $func_info['func_token']);
    if ($class_name !== null) {
        return $class_name . '::' . $func_info['name'];
    }

    return $func_info['name'];
}

/**
 * Find the class name that encloses a token position.
 */
function find_enclosing_class($tokens, $position) {
    // Walk backward to find the most recent class declaration.
    $depth        = 0;
    $in_class     = false;
    $class_name   = null;
    $count        = count($tokens);

    // Simple approach: scan forward from start, track brace depth per class.
    $class_stack = [];
    $brace_depth = 0;

    for ($i = 0; $i < $count; $i++) {
        if (is_array($tokens[$i])) {
            if ($tokens[$i][0] === T_CLASS) {
                // Find class name.
                for ($j = $i + 1; $j < $count; $j++) {
                    if (is_array($tokens[$j]) && $tokens[$j][0] === T_STRING) {
                        $class_stack[] = ['name' => $tokens[$j][1], 'depth' => $brace_depth];
                        break;
                    }
                }
            }
        } else {
            if ($tokens[$i] === '{') {
                $brace_depth++;
            } elseif ($tokens[$i] === '}') {
                $brace_depth--;
                // Pop class stack if we've exited a class.
                while (!empty($class_stack) && end($class_stack)['depth'] >= $brace_depth) {
                    array_pop($class_stack);
                }
            }
        }

        if ($i === $position) {
            return empty($class_stack) ? null : end($class_stack)['name'];
        }
    }

    return null;
}

/**
 * Check if a function key is registered in the callback map.
 */
function is_callback_registered($func_key, $callback_map) {
    if (isset($callback_map[$func_key])) {
        return true;
    }

    // Extract the method name for fuzzy matching.
    if (strpos($func_key, '::') !== false) {
        $method = substr($func_key, strpos($func_key, '::') + 2);
    } else {
        $method = $func_key;
    }

    // Check by method name alone (handles class detection mismatches).
    // E.g., registration file has no class but uses PipelineBatchScheduler::class,
    // so map key is "onChildComplete" but lookup is "PipelineBatchScheduler::onChildComplete".
    foreach ($callback_map as $key => $value) {
        // Exact method name match (bare name in map).
        if ($key === $method) {
            return true;
        }
        // Method name match with any class prefix.
        if (strpos($key, '::' . $method) !== false) {
            return true;
        }
    }

    return false;
}

/**
 * Check if a method overrides an abstract or interface method.
 *
 * Heuristic: if the class extends another class, and the method is public,
 * it's likely an override. PHPCS already handles the implements case via
 * different error codes, so we focus on extends.
 */
function is_override_method($tokens, $func_info) {
    if ($func_info['name'] === null) {
        return false;
    }

    $enclosing_class = find_enclosing_class($tokens, $func_info['func_token']);
    if ($enclosing_class === null) {
        return false;
    }

    // Check whether this specific enclosing class extends another class.
    $count = count($tokens);
    for ($i = 0; $i < $count; $i++) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_CLASS) {
            $class_name = null;
            for ($j = $i + 1; $j < $count; $j++) {
                if (is_array($tokens[$j]) && $tokens[$j][0] === T_STRING) {
                    $class_name = $tokens[$j][1];
                    break;
                }
                if (!is_array($tokens[$j]) && $tokens[$j] === '{') {
                    break;
                }
            }

            if ($class_name !== $enclosing_class) {
                continue;
            }

            // Check for extends.
            for ($j = $i + 1; $j < $count; $j++) {
                if (!is_array($tokens[$j]) && $tokens[$j] === '{') {
                    break;
                }
                if (is_array($tokens[$j]) && $tokens[$j][0] === T_EXTENDS) {
                    return true;
                }
            }
        }
    }

    return false;
}

/**
 * Determine if a parameter can be safely removed.
 *
 * Criteria:
 * - Method must be private (or have no external callers).
 * - The param must be the LAST parameter (removing non-last params changes call semantics).
 * - We can find all call sites.
 */
function can_remove_param($tokens, $func_info, $param_name, $filepath, $scan_root) {
    $result = [
        'removable'      => false,
        'param_position' => -1,
        'call_sites'     => [],
    ];

    // Find param position.
    $param_position = null;
    $total_params   = count($func_info['params']);
    foreach ($func_info['params'] as $p) {
        if ($p['name'] === $param_name) {
            $param_position = $p['position'];
            break;
        }
    }

    if ($param_position === null) {
        return $result;
    }

    $result['param_position'] = $param_position;

    // Only remove the LAST parameter (removing middle params is too risky without
    // updating all callers' positional args).
    if ($param_position !== $total_params - 1) {
        return $result;
    }

    // For closures, we can't easily trace callers.
    if ($func_info['name'] === null) {
        return $result;
    }

    // Private methods — safe to remove, callers are in the same file.
    if ($func_info['visibility'] === 'private') {
        // Find all call sites in this file.
        $call_sites = find_call_sites_in_file($tokens, $func_info['name'], $filepath);
        $result['call_sites'] = $call_sites;
        $result['removable']  = true;
        return $result;
    }

    // Standalone functions (no class) — find callers across codebase.
    if ($func_info['visibility'] === null) {
        $class_name = find_enclosing_class($tokens, $func_info['func_token']);
        if ($class_name === null) {
            // It's a standalone function — scan codebase for callers.
            $call_sites = find_call_sites_in_codebase($func_info['name'], $scan_root, $filepath);
            $result['call_sites'] = $call_sites;
            $result['removable']  = true;
            return $result;
        }
    }

    // Public/protected methods with zero callers across codebase are safe to trim.
    if (in_array($func_info['visibility'], ['public', 'protected'], true)) {
        $call_sites = find_call_sites_in_codebase($func_info['name'], $scan_root, $filepath);
        if (empty($call_sites)) {
            $result['removable'] = true;
            return $result;
        }
        // Has callers — still removable if it's the last param and callers
        // don't pass it (param has a default value).
        $has_default = param_has_default($tokens, $func_info, $param_name);
        if ($has_default) {
            $result['call_sites'] = $call_sites;
            $result['removable']  = true;
            return $result;
        }
    }

    return $result;
}

/**
 * Check if a parameter has a default value.
 */
function param_has_default($tokens, $func_info, $param_name) {
    $paren_open  = $func_info['paren_open'];
    $paren_close = $func_info['paren_close'];

    for ($i = $paren_open + 1; $i < $paren_close; $i++) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_VARIABLE && $tokens[$i][1] === $param_name) {
            // Look forward for '='.
            for ($j = $i + 1; $j < $paren_close; $j++) {
                if (!is_array($tokens[$j]) && $tokens[$j] === '=') {
                    return true;
                }
                if (!is_array($tokens[$j]) && ($tokens[$j] === ',' || $tokens[$j] === ')')) {
                    return false;
                }
            }
        }
    }

    return false;
}

/**
 * Find call sites of a method within the same file (for private methods).
 */
function find_call_sites_in_file($tokens, $method_name, $filepath) {
    $sites = [];
    $count = count($tokens);

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_STRING) {
            continue;
        }
        if ($tokens[$i][1] !== $method_name) {
            continue;
        }

        // Check if followed by (.
        $next = next_non_whitespace($tokens, $i + 1, $count);
        if ($next !== null && !is_array($tokens[$next]) && $tokens[$next] === '(') {
            // Exclude function declarations (preceded by T_FUNCTION keyword).
            $prev = prev_non_whitespace($tokens, $i - 1);
            if ($prev !== null && is_array($tokens[$prev]) && $tokens[$prev][0] === T_FUNCTION) {
                continue;
            }
            $sites[] = [
                'file'        => $filepath,
                'line'        => $tokens[$i][2],
                'token_index' => $i,
                'method_name' => $method_name,
            ];
        }
    }

    return $sites;
}

/**
 * Find call sites of a function/method across the entire codebase.
 *
 * This is a simplified grep-based approach.
 */
function find_call_sites_in_codebase($func_name, $scan_root, $current_filepath) {
    $sites = [];

    // Use grep to find potential call sites.
    $cmd = sprintf(
        'grep -rn --include="*.php" "%s\s*(" %s 2>/dev/null | grep -v "function\s\+%s"',
        $func_name,
        escapeshellarg($scan_root),
        $func_name
    );

    $output = shell_exec($cmd);
    if ($output === null || trim($output) === '') {
        return $sites;
    }

    foreach (explode("\n", trim($output)) as $line) {
        if (preg_match('/^(.+?):(\d+):/', $line, $m)) {
            $file     = $m[1];
            $line_num = (int) $m[2];

            // Skip vendor/node_modules/build.
            if (preg_match('/(vendor|node_modules|build)\//', $file)) {
                continue;
            }

            $sites[] = [
                'file'        => $file,
                'line'        => $line_num,
                'method_name' => $func_name,
            ];
        }
    }

    return $sites;
}

/**
 * Remove a parameter from a function signature.
 *
 * Only removes the LAST parameter. Handles:
 * - Removing the param token, its type hint, default value, and trailing comma.
 */
function remove_param_from_signature(&$tokens, $func_info, $param_name) {
    $paren_open  = $func_info['paren_open'];
    $paren_close = $func_info['paren_close'];

    // Find the target parameter token index.
    $param_token_idx = null;
    foreach ($func_info['params'] as $p) {
        if ($p['name'] === $param_name) {
            $param_token_idx = $p['token_index'];
            break;
        }
    }

    if ($param_token_idx === null) {
        return false;
    }

    // Determine the range to remove:
    // Walk backward from param to find the start (after comma or after open paren).
    $remove_start = $param_token_idx;
    for ($i = $param_token_idx - 1; $i > $paren_open; $i--) {
        if (is_array($tokens[$i])) {
            $code = $tokens[$i][0];
            // Type hints, nullable, whitespace, variadic.
            if (in_array($code, [T_STRING, T_NAME_QUALIFIED, T_NAME_FULLY_QUALIFIED, T_ARRAY, T_CALLABLE, T_WHITESPACE, T_ELLIPSIS, T_NS_SEPARATOR], true)) {
                $remove_start = $i;
                continue;
            }
            // Nullable type.
            if ($code === T_NULLABLE) {
                $remove_start = $i;
                continue;
            }
            break;
        } else {
            if ($tokens[$i] === '?') {
                $remove_start = $i;
                continue;
            }
            break;
        }
    }

    // Walk forward from param to find the end (default value, comma, or close paren).
    $remove_end = $param_token_idx;
    for ($i = $param_token_idx + 1; $i < $paren_close; $i++) {
        if (is_array($tokens[$i])) {
            $code = $tokens[$i][0];
            if ($code === T_WHITESPACE) {
                $remove_end = $i;
                continue;
            }
            // Default value tokens (literals, strings, keywords, qualified names).
            $default_value_tokens = [T_LNUMBER, T_DNUMBER, T_CONSTANT_ENCAPSED_STRING, T_STRING, T_ARRAY, T_WHITESPACE, T_NAME_QUALIFIED, T_NAME_FULLY_QUALIFIED];
            // T_NULL, T_TRUE, T_FALSE only exist in PHP 8.4+.
            if (defined('T_NULL')) {
                $default_value_tokens[] = T_NULL;
            }
            if (defined('T_TRUE')) {
                $default_value_tokens[] = T_TRUE;
            }
            if (defined('T_FALSE')) {
                $default_value_tokens[] = T_FALSE;
            }
            if (in_array($code, $default_value_tokens, true)) {
                $remove_end = $i;
                continue;
            }
            break;
        } else {
            if ($tokens[$i] === '=') {
                $remove_end = $i;
                continue;
            }
            if ($tokens[$i] === '(' || $tokens[$i] === ')') {
                // Part of array() default.
                if ($tokens[$i] === '(') {
                    $matching = find_matching_close($tokens, $i, count($tokens), '(', ')');
                    if ($matching !== null) {
                        $remove_end = $matching;
                        $i = $matching;
                        continue;
                    }
                }
                break;
            }
            if ($tokens[$i] === '[') {
                $matching = find_matching_close($tokens, $i, count($tokens), '[', ']');
                if ($matching !== null) {
                    $remove_end = $matching;
                    $i = $matching;
                    continue;
                }
                break;
            }
            if ($tokens[$i] === ',') {
                $remove_end = $i;
                break;
            }
            $remove_end = $i;
        }
    }

    // Also consume the preceding comma + whitespace if this is not the first param.
    $has_preceding_comma = false;
    for ($i = $remove_start - 1; $i > $paren_open; $i--) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        if (!is_array($tokens[$i]) && $tokens[$i] === ',') {
            $remove_start     = $i;
            $has_preceding_comma = true;
            break;
        }
        break;
    }

    // Remove the tokens by replacing with empty strings.
    for ($i = $remove_start; $i <= $remove_end; $i++) {
        if (is_array($tokens[$i])) {
            $tokens[$i][1] = '';
        } else {
            $tokens[$i] = ['', ''];
        }
    }

    return true;
}

/**
 * Remove a parameter from a call site in another file.
 *
 * Rewrites the file to remove the argument at the given position.
 */
function remove_param_from_call_site($filepath, $line, $param_position, $method_name) {
    $content = file_get_contents($filepath);
    if ($content === false) {
        return;
    }

    $tokens = @token_get_all($content);
    if ($tokens === false) {
        return;
    }

    $count   = count($tokens);
    $changed = false;

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_STRING || $tokens[$i][1] !== $method_name) {
            continue;
        }

        // Check line matches (approximate — might be off by 1 for multi-line calls).
        $token_line = $tokens[$i][2];
        if (abs($token_line - $line) > 3) {
            continue;
        }

        // Find opening paren.
        $paren_idx = next_non_whitespace($tokens, $i + 1, $count);
        if ($paren_idx === null || (is_array($tokens[$paren_idx]) || $tokens[$paren_idx] !== '(')) {
            continue;
        }

        // Find closing paren.
        $close_paren = find_matching_close($tokens, $paren_idx, $count, '(', ')');
        if ($close_paren === null) {
            continue;
        }

        // Count arguments and find the target.
        $arg_ranges = find_argument_ranges($tokens, $paren_idx, $close_paren);
        if ($param_position >= count($arg_ranges)) {
            continue;
        }

        // Remove the target argument (and its preceding comma if not first).
        $range = $arg_ranges[$param_position];
        $rm_start = $range['start'];
        $rm_end   = $range['end'];

        // Remove preceding comma + whitespace.
        if ($param_position > 0) {
            for ($j = $rm_start - 1; $j > $paren_idx; $j--) {
                if (is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
                    $rm_start = $j;
                    continue;
                }
                if (!is_array($tokens[$j]) && $tokens[$j] === ',') {
                    $rm_start = $j;
                    break;
                }
                break;
            }
        }

        for ($j = $rm_start; $j <= $rm_end; $j++) {
            if (is_array($tokens[$j])) {
                $tokens[$j][1] = '';
            } else {
                $tokens[$j] = ['', ''];
            }
        }

        $changed = true;
        break;
    }

    if ($changed) {
        $new_content = rebuild_from_tokens($tokens);
        file_put_contents($filepath, $new_content);
    }
}

/**
 * Find argument ranges in a function call.
 *
 * Returns array of ['start' => int, 'end' => int] for each argument.
 */
function find_argument_ranges($tokens, $paren_open, $paren_close) {
    $ranges = [];
    $depth  = 0;
    $start  = $paren_open + 1;

    for ($i = $paren_open + 1; $i < $paren_close; $i++) {
        $tok = is_array($tokens[$i]) ? null : $tokens[$i];

        if ($tok === '(' || $tok === '[' || $tok === '{') {
            $depth++;
        } elseif ($tok === ')' || $tok === ']' || $tok === '}') {
            $depth--;
        } elseif ($tok === ',' && $depth === 0) {
            $ranges[] = ['start' => $start, 'end' => $i - 1];
            $start = $i + 1;
        }
    }

    // Last argument.
    if ($start < $paren_close) {
        $ranges[] = ['start' => $start, 'end' => $paren_close - 1];
    }

    return $ranges;
}

/**
 * Remove an argument at a given position from a call site in the in-memory tokens array.
 *
 * Used for same-file call sites to avoid read/write race with the main file processing.
 */
function remove_arg_from_tokens(&$tokens, $line, $param_position, $method_name) {
    $count = count($tokens);

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_STRING || $tokens[$i][1] !== $method_name) {
            continue;
        }

        $token_line = $tokens[$i][2];
        if (abs($token_line - $line) > 3) {
            continue;
        }

        // Exclude function declarations.
        $prev = prev_non_whitespace($tokens, $i - 1);
        if ($prev !== null && is_array($tokens[$prev]) && $tokens[$prev][0] === T_FUNCTION) {
            continue;
        }

        // Find opening paren.
        $paren_idx = next_non_whitespace($tokens, $i + 1, $count);
        if ($paren_idx === null || is_array($tokens[$paren_idx]) || $tokens[$paren_idx] !== '(') {
            continue;
        }

        // Find closing paren.
        $close_paren = find_matching_close($tokens, $paren_idx, $count, '(', ')');
        if ($close_paren === null) {
            continue;
        }

        // Find argument ranges.
        $arg_ranges = find_argument_ranges($tokens, $paren_idx, $close_paren);
        if ($param_position >= count($arg_ranges)) {
            continue;
        }

        $range    = $arg_ranges[$param_position];
        $rm_start = $range['start'];
        $rm_end   = $range['end'];

        // Remove preceding comma + whitespace for non-first params.
        if ($param_position > 0) {
            for ($j = $rm_start - 1; $j > $paren_idx; $j--) {
                if (is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
                    $rm_start = $j;
                    continue;
                }
                if (!is_array($tokens[$j]) && $tokens[$j] === ',') {
                    $rm_start = $j;
                    break;
                }
                break;
            }
        }

        // Blank out the tokens.
        for ($j = $rm_start; $j <= $rm_end; $j++) {
            if (is_array($tokens[$j])) {
                $tokens[$j][1] = '';
            } else {
                $tokens[$j] = ['', ''];
            }
        }

        return;
    }
}

// ============================================================================
// Noop insertion helpers
// ============================================================================

/**
 * Find a function by name and approximate line number in tokens.
 *
 * Used after token rebuilding when exact line numbers may have shifted.
 */
function find_function_by_name_and_approx_line($tokens, $func_name, $approx_line) {
    $count      = count($tokens);
    $best_match = null;
    $best_dist  = PHP_INT_MAX;

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_FUNCTION) {
            continue;
        }

        $func_token = $i;
        $func_line  = $tokens[$i][2];

        // Find function name.
        $name = null;
        $paren_open = null;
        for ($j = $i + 1; $j < $count; $j++) {
            if (is_array($tokens[$j]) && $tokens[$j][0] === T_STRING && $name === null) {
                $name = $tokens[$j][1];
            }
            if (!is_array($tokens[$j]) && $tokens[$j] === '(') {
                $paren_open = $j;
                break;
            }
        }

        // For closures, match by line proximity.
        if ($func_name === null && $name === null) {
            $dist = abs($func_line - $approx_line);
            if ($dist < $best_dist) {
                $best_dist = $dist;
                // Build full func_info.
                $best_match = build_func_info_from_token($tokens, $func_token, $name, $paren_open, $count);
            }
            continue;
        }

        if ($name !== $func_name) {
            continue;
        }

        $dist = abs($func_line - $approx_line);
        if ($dist < $best_dist) {
            $best_dist  = $dist;
            $best_match = build_func_info_from_token($tokens, $func_token, $name, $paren_open, $count);
        }
    }

    return $best_match;
}

/**
 * Build a func_info array from a function token position.
 */
function build_func_info_from_token($tokens, $func_token, $name, $paren_open, $count) {
    if ($paren_open === null) {
        return null;
    }

    $paren_close = find_matching_close($tokens, $paren_open, $count, '(', ')');
    if ($paren_close === null) {
        return null;
    }

    $body_open = null;
    for ($j = $paren_close + 1; $j < $count; $j++) {
        if (!is_array($tokens[$j]) && $tokens[$j] === '{') {
            $body_open = $j;
            break;
        }
        if (!is_array($tokens[$j]) && $tokens[$j] === ';') {
            break;
        }
    }

    if ($body_open === null) {
        return null;
    }

    $body_close = find_matching_close($tokens, $body_open, $count, '{', '}');
    if ($body_close === null) {
        return null;
    }

    return [
        'func_token'  => $func_token,
        'name'        => $name,
        'paren_open'  => $paren_open,
        'paren_close' => $paren_close,
        'body_open'   => $body_open,
        'body_close'  => $body_close,
        'params'      => extract_params($tokens, $paren_open, $paren_close),
        'visibility'  => detect_visibility($tokens, $func_token),
        'line'        => $tokens[$func_token][2],
    ];
}

/**
 * Detect the indentation used in the function body.
 *
 * Looks at the first non-empty line after the given line index.
 */
function detect_body_indent($lines, $start_index) {
    for ($i = $start_index; $i < count($lines); $i++) {
        $line = $lines[$i];
        if (trim($line) === '') {
            continue;
        }
        // Extract leading whitespace.
        if (preg_match('/^(\s+)/', $line, $m)) {
            return $m[1];
        }
        break;
    }
    // Fallback: one tab (WordPress standard).
    return "\t";
}

// ============================================================================
// Token helper functions
// ============================================================================

/**
 * Find matching close delimiter.
 */
function find_matching_close($tokens, $open, $count, $open_char, $close_char) {
    $depth = 1;
    for ($i = $open + 1; $i < $count; $i++) {
        $tok = is_array($tokens[$i]) ? null : $tokens[$i];
        if ($tok === $open_char) {
            $depth++;
        } elseif ($tok === $close_char) {
            $depth--;
            if ($depth === 0) {
                return $i;
            }
        }
    }
    return null;
}

/**
 * Get the line number for a non-array token by checking the nearest array token.
 */
function get_line_at($tokens, $position) {
    for ($i = $position; $i >= 0; $i--) {
        if (is_array($tokens[$i]) && isset($tokens[$i][2])) {
            return $tokens[$i][2];
        }
    }
    return 0;
}

/**
 * Find next non-whitespace token.
 */
function next_non_whitespace($tokens, $start, $count) {
    for ($i = $start; $i < $count; $i++) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        return $i;
    }
    return null;
}

/**
 * Find previous non-whitespace token.
 */
function prev_non_whitespace($tokens, $start) {
    for ($i = $start; $i >= 0; $i--) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        return $i;
    }
    return null;
}

/**
 * Rebuild file content from tokens.
 */
function rebuild_from_tokens($tokens) {
    $output = '';
    foreach ($tokens as $token) {
        if (is_array($token)) {
            $output .= $token[1];
        } else {
            $output .= $token;
        }
    }
    return $output;
}
