#!/usr/bin/env php
<?php
/**
 * Lonely If Fixer
 *
 * Transforms `else { if (...) { ... } }` into `elseif (...) { ... }` when the
 * if statement is the only statement inside the else block.
 *
 * Handles comments between else and if by preserving them above the elseif.
 *
 * Usage: php lonely-if-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ($argc < 2) {
    echo "Usage: php lonely-if-fixer.php <path>\n";
    exit(1);
}

$path = $argv[1];

if (!file_exists($path)) {
    echo "Error: Path not found: $path\n";
    exit(1);
}

$result = fixer_process_path($path, 'process_file');

if ($result['total_fixes'] > 0) {
    echo "Lonely if fixer: Fixed {$result['total_fixes']} else-if(s) in {$result['files_fixed']} file(s)\n";
} else {
    echo "Lonely if fixer: No fixable patterns found\n";
}

exit(0);

/**
 * Process a single PHP file using line-based approach.
 *
 * Line-based is simpler for this structural change because we need to handle
 * indentation adjustments (the if body was indented one extra level inside else).
 */
function process_file($filepath) {
    $lines = file($filepath);
    if ($lines === false) {
        return 0;
    }

    $fixes = 0;
    $new_lines = [];
    $count = count($lines);
    $i = 0;

    while ($i < $count) {
        $line = $lines[$i];

        // Look for: } else {
        if (preg_match('/^(\s*)\}\s*else\s*\{/', $line, $else_match)) {
            $else_indent = $else_match[1];
            $inner_indent = $else_indent . "\t"; // One level deeper

            // Collect lines inside the else block
            $inner_lines = [];
            $comments = [];
            $j = $i + 1;

            // Collect comments and whitespace before the if
            while ($j < $count) {
                $inner = $lines[$j];
                $trimmed = trim($inner);
                if ($trimmed === '' || $trimmed === '') {
                    $j++;
                    continue;
                }
                if (strpos($trimmed, '//') === 0 || strpos($trimmed, '/*') === 0) {
                    $comments[] = $inner;
                    $j++;
                    continue;
                }
                break;
            }

            // Check if the next non-comment line is an if statement
            if ($j < $count && preg_match('/^\s*if\s*\(/', $lines[$j])) {
                // Find the end of the if block (including elseif/else chains)
                $if_start = $j;
                $brace_depth = 0;
                $if_end = null;
                $in_if_chain = true;

                for ($k = $if_start; $k < $count && $in_if_chain; $k++) {
                    $check = $lines[$k];
                    // Count braces
                    $brace_depth += substr_count($check, '{') - substr_count($check, '}');

                    if ($brace_depth === 0) {
                        // Check if next non-empty line starts with elseif/else
                        $peek = $k + 1;
                        while ($peek < $count && trim($lines[$peek]) === '') {
                            $peek++;
                        }

                        if ($peek < $count && preg_match('/^\s*\}\s*(elseif|else)\b/', $lines[$peek])) {
                            continue; // Part of the chain
                        }

                        $if_end = $k;
                        $in_if_chain = false;
                    }
                }

                if ($if_end !== null) {
                    // Check that the line after the if block is the closing brace of else
                    $after_if = $if_end + 1;
                    while ($after_if < $count && trim($lines[$after_if]) === '') {
                        $after_if++;
                    }

                    if ($after_if < $count && preg_match('/^\s*\}\s*$/', $lines[$after_if])) {
                        // This is a lonely if! Transform it.
                        $fixes++;

                        // Emit comments (dedented to else level)
                        foreach ($comments as $comment) {
                            $dedented = dedent_line($comment, $inner_indent, $else_indent);
                            $new_lines[] = $dedented;
                        }

                        // Change "} else {" + "if (" to "} elseif ("
                        $first_if_line = $lines[$if_start];
                        // Replace leading if with elseif, dedent
                        $first_if_line = preg_replace('/^\s*if\b/', $else_indent . '} elseif', $first_if_line, 1);
                        $new_lines[] = $first_if_line;

                        // Emit the rest of the if body (dedented by one level)
                        for ($k = $if_start + 1; $k <= $if_end; $k++) {
                            $new_lines[] = dedent_line($lines[$k], $inner_indent, $else_indent);
                        }

                        // Skip past the closing brace of the else block
                        $i = $after_if + 1;
                        continue;
                    }
                }
            }
        }

        $new_lines[] = $line;
        $i++;
    }

    if ($fixes > 0) {
        file_put_contents($filepath, implode('', $new_lines));
    }

    return $fixes;
}

/**
 * Dedent a line by replacing inner_indent with outer_indent at the start.
 */
function dedent_line($line, $inner_indent, $outer_indent) {
    if (strpos($line, $inner_indent) === 0) {
        return $outer_indent . substr($line, strlen($inner_indent));
    }
    return $line;
}
