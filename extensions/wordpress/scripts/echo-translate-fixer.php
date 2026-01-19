#!/usr/bin/env php
<?php
/**
 * Echo Translate Fixer
 *
 * Converts echo __() to echo esc_html__() for WordPress coding standards compliance.
 * WordPress requires escaping output from translation functions.
 *
 * Usage: php echo-translate-fixer.php <path>
 */

if ($argc < 2) {
    echo "Usage: php echo-translate-fixer.php <path>\n";
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
    echo "echo-translate fixer: Fixed $total_fixes call(s) in $files_fixed file(s)\n";
} else {
    echo "echo-translate fixer: No fixable calls found\n";
}

exit(0);

/**
 * Process a single PHP file and fix echo __() calls.
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
    $after_echo = false;

    while ($i < $count) {
        $token = $tokens[$i];

        // Track when we see an echo statement
        if (is_array($token) && $token[0] === T_ECHO) {
            $after_echo = true;
            $new_content .= token_to_string($token);
            $i++;
            continue;
        }

        // Reset after_echo on semicolon or other statement terminators
        if (!is_array($token) && in_array($token, [';', '{', '}'], true)) {
            $after_echo = false;
        }

        // Look for __ function call after echo
        if ($after_echo && is_array($token) && $token[0] === T_STRING && $token[1] === '__') {
            // Check that it's followed by ( to confirm it's a function call
            $j = $i + 1;
            while ($j < $count && is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
                $j++;
            }

            if ($j < $count && $tokens[$j] === '(') {
                // Check that it's not preceded by another identifier (like some__)
                $prev = $i - 1;
                while ($prev >= 0 && is_array($tokens[$prev]) && $tokens[$prev][0] === T_WHITESPACE) {
                    $prev--;
                }

                $is_standalone = true;
                if ($prev >= 0) {
                    $prev_token = $tokens[$prev];
                    // If preceded by T_STRING, T_OBJECT_OPERATOR, T_DOUBLE_COLON, it's not standalone __
                    if (is_array($prev_token) && in_array($prev_token[0], [T_STRING, T_OBJECT_OPERATOR, T_DOUBLE_COLON, T_PAAMAYIM_NEKUDOTAYIM], true)) {
                        $is_standalone = false;
                    }
                }

                if ($is_standalone) {
                    // Replace __ with esc_html__
                    $new_content .= 'esc_html__';
                    $fixes++;
                    $i++;
                    continue;
                }
            }
        }

        // Skip whitespace in the after_echo check but keep tracking
        if (is_array($token) && $token[0] === T_WHITESPACE) {
            $new_content .= token_to_string($token);
            $i++;
            continue;
        }

        // Reset after_echo if we hit something other than __ or whitespace
        if ($after_echo && !(is_array($token) && $token[0] === T_STRING && $token[1] === '__')) {
            // Only reset if it's not the string concatenation operator
            if (!is_array($token) || $token[0] !== T_WHITESPACE) {
                // Don't reset for concatenation operators within echo
                if ($token !== '.') {
                    $after_echo = false;
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
