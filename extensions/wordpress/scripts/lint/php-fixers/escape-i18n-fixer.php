#!/usr/bin/env php
<?php
/**
 * Escape i18n Fixer
 *
 * Converts _e() to esc_html_e() for WordPress coding standards compliance.
 * WordPress requires escaping output from translation functions.
 *
 * Usage: php escape-i18n-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php escape-i18n-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "escape-i18n fixer: Fixed {$result['total_fixes']} call(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "escape-i18n fixer: No fixable calls found\n";
}

exit(0);

/**
 * Process a single PHP file and fix _e calls.
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

        // Look for _e function call (exact match, not part of another function name)
        if (is_array($token) && $token[0] === T_STRING && $token[1] === '_e') {
            // Check that it's followed by ( to confirm it's a function call
            $j = $i + 1;
            while ($j < $count && is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
                $j++;
            }

            if ($j < $count && $tokens[$j] === '(') {
                // Check that it's not preceded by another identifier (like some_e)
                // by looking at the previous non-whitespace token
                $prev = $i - 1;
                while ($prev >= 0 && is_array($tokens[$prev]) && $tokens[$prev][0] === T_WHITESPACE) {
                    $prev--;
                }

                $is_standalone = true;
                if ($prev >= 0) {
                    $prev_token = $tokens[$prev];
                    // If preceded by T_STRING, T_OBJECT_OPERATOR, T_DOUBLE_COLON, it's not standalone _e
                    if (is_array($prev_token) && in_array($prev_token[0], [T_STRING, T_OBJECT_OPERATOR, T_DOUBLE_COLON, T_PAAMAYIM_NEKUDOTAYIM], true)) {
                        $is_standalone = false;
                    }
                }

                if ($is_standalone) {
                    // Replace _e with esc_html_e
                    $new_content .= 'esc_html_e';
                    $fixes++;
                    $i++;
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
 * Convert token to string.
 */
function token_to_string($token) {
    return is_array($token) ? $token[1] : $token;
}
