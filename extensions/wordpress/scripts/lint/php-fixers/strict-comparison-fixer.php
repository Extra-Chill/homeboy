#!/usr/bin/env php
<?php
/**
 * Strict Comparison Fixer
 *
 * Converts loose comparisons to strict:
 *   == → ===
 *   != → !==
 *
 * WPCS marks Universal.Operators.StrictComparisons as phpcs-only (phpcbf won't fix it)
 * because loose-to-strict can change behavior. This fixer handles it since the codebase
 * has opted into strict comparisons via the WordPress coding standard.
 *
 * Usage: php strict-comparison-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php strict-comparison-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "Strict comparison fixer: Fixed {$result['total_fixes']} comparison(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "Strict comparison fixer: No fixable comparisons found\n";
}

exit(0);

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

    $fixes = 0;
    $new_content = '';
    $count = count($tokens);

    for ($i = 0; $i < $count; $i++) {
        $token = $tokens[$i];

        if (is_array($token)) {
            // == → ===
            if ($token[0] === T_IS_EQUAL) {
                $new_content .= '===';
                $fixes++;
                continue;
            }

            // != → !==
            if ($token[0] === T_IS_NOT_EQUAL) {
                $new_content .= '!==';
                $fixes++;
                continue;
            }

            $new_content .= $token[1];
        } else {
            $new_content .= $token;
        }
    }

    if ($fixes > 0) {
        file_put_contents($filepath, $new_content);
    }

    return $fixes;
}
