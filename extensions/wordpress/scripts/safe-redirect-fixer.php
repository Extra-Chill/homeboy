#!/usr/bin/env php
<?php
/**
 * Safe Redirect Fixer
 *
 * Converts wp_redirect() to wp_safe_redirect() for WordPress coding standards compliance.
 * wp_safe_redirect validates the redirect URL against an allowed hosts whitelist.
 *
 * Usage: php safe-redirect-fixer.php <path>
 */

if ($argc < 2) {
    echo "Usage: php safe-redirect-fixer.php <path>\n";
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
    echo "safe-redirect fixer: Fixed $total_fixes call(s) in $files_fixed file(s)\n";
} else {
    echo "safe-redirect fixer: No fixable calls found\n";
}

exit(0);

/**
 * Process a single PHP file and fix wp_redirect calls.
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

        // Look for wp_redirect function call
        if (is_array($token) && $token[0] === T_STRING && $token[1] === 'wp_redirect') {
            // Check that it's followed by ( to confirm it's a function call
            $j = $i + 1;
            while ($j < $count && is_array($tokens[$j]) && $tokens[$j][0] === T_WHITESPACE) {
                $j++;
            }

            if ($j < $count && $tokens[$j] === '(') {
                // Check that it's not preceded by another identifier
                $prev = $i - 1;
                while ($prev >= 0 && is_array($tokens[$prev]) && $tokens[$prev][0] === T_WHITESPACE) {
                    $prev--;
                }

                $is_standalone = true;
                if ($prev >= 0) {
                    $prev_token = $tokens[$prev];
                    // If preceded by T_STRING, T_OBJECT_OPERATOR, T_DOUBLE_COLON, it's not standalone
                    if (is_array($prev_token) && in_array($prev_token[0], [T_STRING, T_OBJECT_OPERATOR, T_DOUBLE_COLON, T_PAAMAYIM_NEKUDOTAYIM], true)) {
                        $is_standalone = false;
                    }
                }

                if ($is_standalone) {
                    // Replace wp_redirect with wp_safe_redirect
                    $new_content .= 'wp_safe_redirect';
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
