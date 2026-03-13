#!/usr/bin/env php
<?php
/**
 * WordPress Text Domain Fixer
 *
 * Fixes WordPress.WP.I18n.TextDomainMismatch errors by replacing incorrect
 * text domain strings in i18n function calls with the correct domain from
 * the plugin's header.
 *
 * This is fully deterministic: the correct text domain is declared in the
 * plugin header ("Text Domain: slug"), and every i18n call in that plugin
 * should use it. No human judgment needed.
 *
 * Handles all WordPress i18n functions:
 *   __(), _e(), _x(), _ex(), _n(), _nx(), _n_noop(), _nx_noop(),
 *   esc_html__(), esc_html_e(), esc_html_x(),
 *   esc_attr__(), esc_attr_e(), esc_attr_x()
 *
 * Usage: php text-domain-fixer.php <path> [--text-domain=<domain>]
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php text-domain-fixer.php <path> [--text-domain=<domain>]\n";
    exit(1);
}

$target_path = $argv[1];

if (!file_exists($target_path)) {
    echo "Error: Path not found: $target_path\n";
    exit(1);
}

// Parse optional arguments.
$text_domain = null;
for ($i = 2; $i < $argc; $i++) {
    if (strpos($argv[$i], '--text-domain=') === 0) {
        $text_domain = substr($argv[$i], strlen('--text-domain='));
    }
}

// Auto-detect text domain from plugin header if not provided.
if ($text_domain === null) {
    $text_domain = detect_text_domain($target_path);
}

if ($text_domain === null || $text_domain === '') {
    echo "Error: Could not detect text domain. Use --text-domain=<domain> or add 'Text Domain:' to your plugin header.\n";
    exit(1);
}

echo "Text domain: $text_domain\n";

// Process files
$GLOBALS['correct_domain'] = $text_domain;
$result = fixer_process_path($target_path, 'fix_text_domain_in_file');

if ($result['total_fixes'] > 0) {
    echo "Text domain fixer: Fixed {$result['total_fixes']} text domain(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "Text domain fixer: No text domain mismatches found\n";
}

exit(0);

/**
 * Detect the correct text domain from the plugin header.
 *
 * Searches for the main plugin file (contains "Plugin Name:" in header)
 * and extracts the "Text Domain:" value.
 *
 * @param string $path File or directory path.
 * @return string|null The text domain, or null if not found.
 */
function detect_text_domain($path) {
    $search_dir = is_dir($path) ? $path : dirname($path);

    // Look for plugin files in the root of the path (maxdepth 1).
    $candidates = glob($search_dir . '/*.php');
    if ($candidates === false) {
        return null;
    }

    foreach ($candidates as $file) {
        $header = file_get_contents($file, false, null, 0, 8192);
        if ($header === false) {
            continue;
        }

        // Must have "Plugin Name:" to be a plugin header file.
        if (stripos($header, 'Plugin Name:') === false) {
            continue;
        }

        // Extract "Text Domain: slug" from the header.
        if (preg_match('/Text\s+Domain:\s*(\S+)/i', $header, $m)) {
            return trim($m[1]);
        }
    }

    return null;
}

/**
 * Fix text domain mismatches in a single PHP file.
 *
 * @param string $filepath Path to the PHP file.
 * @return int Number of fixes applied.
 */
function fix_text_domain_in_file($filepath) {
    $content = file_get_contents($filepath);
    if ($content === false) {
        return 0;
    }

    $correct_domain = $GLOBALS['correct_domain'];

    // WordPress i18n functions that take a text domain as the LAST argument.
    // Group 1: domain is the 2nd arg  — __(), _e()
    // Group 2: domain is the 3rd arg  — _x(), _ex(), _n_noop()
    // Group 3: domain is the 4th arg  — _n()
    // Group 4: domain is the 5th arg  — _nx(), _nx_noop()
    // Group 5: domain is the 2nd arg  — esc_html__(), esc_html_e(), esc_attr__(), esc_attr_e()
    // Group 6: domain is the 3rd arg  — esc_html_x(), esc_attr_x()
    //
    // Rather than tracking arg positions, we use a simpler approach:
    // For all these functions, the text domain is ALWAYS the LAST string argument.
    // We find the function call, locate the last string argument, and check if
    // it matches the correct domain.

    // Pattern matches i18n function calls.
    // We use token-based parsing for accuracy.
    $tokens = @token_get_all($content);
    if ($tokens === false) {
        return 0;
    }

    $i18n_functions = [
        '__', '_e', '_x', '_ex', '_n', '_nx', '_n_noop', '_nx_noop',
        'esc_html__', 'esc_html_e', 'esc_html_x',
        'esc_attr__', 'esc_attr_e', 'esc_attr_x',
    ];
    $i18n_set = array_flip($i18n_functions);

    $count  = count($tokens);
    $fixes  = 0;

    for ($i = 0; $i < $count; $i++) {
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_STRING) {
            continue;
        }

        $func_name = $tokens[$i][1];
        if (!isset($i18n_set[$func_name])) {
            continue;
        }

        // Make sure this is a function call (followed by '(')
        $paren_idx = find_next_non_ws($tokens, $i + 1, $count);
        if ($paren_idx === null || is_array($tokens[$paren_idx]) || $tokens[$paren_idx] !== '(') {
            continue;
        }

        // Make sure this is not a function declaration
        $prev_idx = find_prev_non_ws($tokens, $i - 1);
        if ($prev_idx !== null && is_array($tokens[$prev_idx]) && $tokens[$prev_idx][0] === T_FUNCTION) {
            continue;
        }

        // Find the closing paren
        $close_paren = find_close_paren($tokens, $paren_idx, $count);
        if ($close_paren === null) {
            continue;
        }

        // Find the last string literal argument (the text domain).
        // Walk backward from close paren to find the last T_CONSTANT_ENCAPSED_STRING.
        $domain_token_idx = find_last_string_arg($tokens, $paren_idx, $close_paren);
        if ($domain_token_idx === null) {
            continue;
        }

        // Extract the current domain value (strip quotes).
        $current_value = $tokens[$domain_token_idx][1];
        $quote_char    = $current_value[0]; // ' or "
        $current_domain = substr($current_value, 1, -1);

        // Skip if already correct.
        if ($current_domain === $correct_domain) {
            continue;
        }

        // Skip if this doesn't look like a text domain (contains variables, expressions, etc.)
        if (strpos($current_domain, '$') !== false || strpos($current_domain, '{') !== false) {
            continue;
        }

        // Replace with the correct domain.
        $tokens[$domain_token_idx][1] = $quote_char . $correct_domain . $quote_char;
        $fixes++;
    }

    if ($fixes === 0) {
        return 0;
    }

    // Rebuild content from tokens.
    $new_content = '';
    foreach ($tokens as $token) {
        $new_content .= is_array($token) ? $token[1] : $token;
    }

    file_put_contents($filepath, $new_content);
    return $fixes;
}

/**
 * Find the last string literal argument in a function call.
 *
 * Walks backward from the closing paren, skipping whitespace and looking
 * for a T_CONSTANT_ENCAPSED_STRING that appears after a comma (i.e., it's
 * the last argument, not part of a concatenation or array).
 *
 * @return int|null Token index of the last string argument, or null.
 */
function find_last_string_arg($tokens, $paren_open, $paren_close) {
    // Walk backward from close paren to find the last string literal.
    for ($i = $paren_close - 1; $i > $paren_open; $i--) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }

        if (is_array($tokens[$i]) && $tokens[$i][0] === T_CONSTANT_ENCAPSED_STRING) {
            // Verify this string is preceded by a comma (it's a separate argument,
            // not part of a concatenation).
            $prev = find_prev_non_ws($tokens, $i - 1);
            if ($prev !== null && !is_array($tokens[$prev]) && $tokens[$prev] === ',') {
                return $i;
            }
            // If preceded by '(' it's the only/first argument — could be domain
            // for functions like __('text', 'domain') where we want the second arg.
            // In this case, walk further back to check.
        }

        // If we hit something that's not whitespace or a string, stop.
        // The last argument isn't a simple string literal.
        if (!is_array($tokens[$i]) || $tokens[$i][0] !== T_WHITESPACE) {
            break;
        }
    }

    return null;
}

/**
 * Find the next non-whitespace token.
 */
function find_next_non_ws($tokens, $start, $count) {
    for ($i = $start; $i < $count; $i++) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        return $i;
    }
    return null;
}

/**
 * Find the previous non-whitespace token.
 */
function find_prev_non_ws($tokens, $start) {
    for ($i = $start; $i >= 0; $i--) {
        if (is_array($tokens[$i]) && $tokens[$i][0] === T_WHITESPACE) {
            continue;
        }
        return $i;
    }
    return null;
}

/**
 * Find matching closing parenthesis.
 */
function find_close_paren($tokens, $open, $count) {
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
