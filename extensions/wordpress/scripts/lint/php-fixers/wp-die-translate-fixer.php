#!/usr/bin/env php
<?php
/**
 * WP Die Translate Fixer
 *
 * Converts wp_die(__()) to wp_die(esc_html__()) for WordPress coding standards compliance.
 * WordPress requires escaping output passed to wp_die().
 *
 * Usage: php wp-die-translate-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php wp-die-translate-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "wp-die-translate fixer: Fixed {$result['total_fixes']} call(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "wp-die-translate fixer: No fixable calls found\n";
}

exit(0);

/**
 * Process a single PHP file and fix wp_die(__()) calls.
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

        // Look for wp_die function call
        if (is_array($token) && $token[0] === T_STRING && $token[1] === 'wp_die') {
            // Look ahead for opening parenthesis
            $j = $i + 1;
            while ($j < $count && is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
                $j++;
            }

            if ($j < $count && $tokens[$j] === '(') {
                // Found wp_die(, now look for __ as immediate first argument
                $k = $j + 1;
                while ($k < $count && is_array($tokens[$k]) && $tokens[$k][0] === T_WHITESPACE) {
                    $k++;
                }

                if ($k < $count && is_array($tokens[$k]) && $tokens[$k][0] === T_STRING && $tokens[$k][1] === '__') {
                    // Verify __ is followed by (
                    $m = $k + 1;
                    while ($m < $count && is_array($tokens[$m]) && $tokens[$m][0] === T_WHITESPACE) {
                        $m++;
                    }

                    if ($m < $count && $tokens[$m] === '(') {
                        // Found wp_die( __( pattern - output everything up to __ then replace it
                        $new_content .= token_to_string($token); // wp_die
                        $i++;

                        // Output whitespace between wp_die and (
                        while ($i < $k) {
                            $new_content .= token_to_string($tokens[$i]);
                            $i++;
                        }

                        // Replace __ with esc_html__
                        $new_content .= 'esc_html__';
                        $fixes++;
                        $i++; // Skip the __ token
                        continue;
                    }
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
