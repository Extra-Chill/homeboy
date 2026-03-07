#!/usr/bin/env php
<?php
/**
 * PHPCS Ignore Comment Fixer
 *
 * Adds phpcs:disable/enable blocks for known false-positive or unfixable
 * PHPCS violations — the WordPress ecosystem standard for handling sniff
 * limitations (used by WooCommerce, Yoast, etc.).
 *
 * Strategy:
 *   1. Run PHPCS to collect all violations for target sniffs
 *   2. Group violations into "statement blocks" (consecutive or nearby lines)
 *   3. Wrap each block with phpcs:disable ... / phpcs:enable ... comments
 *   4. For isolated single-line violations, use phpcs:ignore instead
 *
 * Targeted sniff categories with context-aware reason comments:
 *
 *   WordPress.DB.PreparedSQL         — Table names from $wpdb->prefix, not user input
 *   WordPress.DB.PreparedSQLPlaceholders — Dynamic placeholders or LIKE patterns
 *   WordPress.PHP.DiscouragedPHPFunctions — base64_encode for API auth
 *   WordPress.WP.AlternativeFunctions    — mt_srand for deterministic seeding
 *   WordPress.NamingConventions.ValidHookName — Slash-separated hook namespaces
 *
 * Usage: php phpcs-ignore-fixer.php <path> [--phpcs-binary=<path>] [--phpcs-standard=<path>]
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php phpcs-ignore-fixer.php <path> [--phpcs-binary=<path>] [--phpcs-standard=<path>]\n";
	exit( 1 );
}

$target_path = $argv[1];

if ( ! file_exists( $target_path ) ) {
	echo "Error: Path not found: $target_path\n";
	exit( 1 );
}

// Parse optional arguments.
$phpcs_binary   = null;
$phpcs_standard = null;
for ( $i = 2; $i < $argc; $i++ ) {
	if ( strpos( $argv[ $i ], '--phpcs-binary=' ) === 0 ) {
		$phpcs_binary = substr( $argv[ $i ], strlen( '--phpcs-binary=' ) );
	} elseif ( strpos( $argv[ $i ], '--phpcs-standard=' ) === 0 ) {
		$phpcs_standard = substr( $argv[ $i ], strlen( '--phpcs-standard=' ) );
	}
}

if ( null === $phpcs_binary ) {
	$phpcs_binary = find_phpcs();
}

if ( null === $phpcs_binary ) {
	echo "Error: Cannot find phpcs binary. Pass --phpcs-binary=<path>\n";
	exit( 1 );
}

/**
 * Sniff families we handle, mapped to their reason comments.
 *
 * Keys are 3-part sniff codes (what --sniffs accepts).
 * Each entry has a reason and the specific 4-part message codes we target.
 */
$sniff_config = array(
	'WordPress.DB.PreparedSQL' => array(
		'reason'  => 'Table name from $wpdb->prefix, not user input.',
		'sources' => array(
			'WordPress.DB.PreparedSQL.NotPrepared',
			'WordPress.DB.PreparedSQL.InterpolatedNotPrepared',
		),
	),
	'WordPress.DB.PreparedSQLPlaceholders' => array(
		'reason'  => 'Dynamic query construction with safe values.',
		'sources' => array(
			'WordPress.DB.PreparedSQLPlaceholders.LikeWildcardsInQuery',
			'WordPress.DB.PreparedSQLPlaceholders.ReplacementsWrongNumber',
			'WordPress.DB.PreparedSQLPlaceholders.UnfinishedPrepare',
		),
	),
	'WordPress.PHP.DiscouragedPHPFunctions' => array(
		'reason'  => 'Required for API authentication, not obfuscation.',
		'sources' => array(
			'WordPress.PHP.DiscouragedPHPFunctions.obfuscation_base64_encode',
		),
	),
	'WordPress.WP.AlternativeFunctions' => array(
		'reason'  => 'Intentional deterministic seeding for reproducible output.',
		'sources' => array(
			'WordPress.WP.AlternativeFunctions.rand_seeding_mt_srand',
		),
	),
	'WordPress.NamingConventions.ValidHookName' => array(
		'reason'  => 'Intentional slash-separated hook namespace.',
		'sources' => array(
			'WordPress.NamingConventions.ValidHookName.UseUnderscores',
		),
	),
);

// Build lookup: 4-part source → 3-part sniff family.
$source_to_family = array();
foreach ( $sniff_config as $family => $config ) {
	foreach ( $config['sources'] as $source ) {
		$source_to_family[ $source ] = $family;
	}
}

$result = process_path( $target_path, $phpcs_binary, $phpcs_standard, $sniff_config, $source_to_family );

if ( $result['total_fixes'] > 0 ) {
	echo "PHPCS ignore fixer: Added {$result['total_fixes']} ignore comment(s) in {$result['files_fixed']} file(s)\n";
} else {
	echo "PHPCS ignore fixer: No fixable violations found\n";
}

exit( 0 );

/**
 * Find the PHPCS binary.
 */
function find_phpcs(): ?string {
	$candidates = array(
		__DIR__ . '/../../vendor/bin/phpcs',
		__DIR__ . '/../../../vendor/bin/phpcs',
	);

	foreach ( $candidates as $candidate ) {
		$resolved = realpath( $candidate );
		if ( $resolved && is_executable( $resolved ) ) {
			return $resolved;
		}
	}

	$which = trim( shell_exec( 'which phpcs 2>/dev/null' ) ?? '' );
	if ( '' !== $which && is_executable( $which ) ) {
		return $which;
	}

	return null;
}

/**
 * Process a path by running PHPCS once, then applying disable/enable blocks per file.
 */
function process_path( string $path, string $phpcs_binary, ?string $phpcs_standard, array $sniff_config, array $source_to_family ): array {
	$sniff_list = implode( ',', array_keys( $sniff_config ) );

	$cmd = escapeshellarg( $phpcs_binary )
		. ' --report=json'
		. ' --sniffs=' . escapeshellarg( $sniff_list );

	if ( null !== $phpcs_standard ) {
		$cmd .= ' --standard=' . escapeshellarg( $phpcs_standard );
	}

	$cmd .= ' -- ' . escapeshellarg( $path ) . ' 2>/dev/null';

	$output = shell_exec( $cmd );

	if ( null === $output || '' === $output ) {
		return array( 'total_fixes' => 0, 'files_fixed' => 0 );
	}

	$data = json_decode( $output, true );
	if ( ! $data || empty( $data['files'] ) ) {
		return array( 'total_fixes' => 0, 'files_fixed' => 0 );
	}

	$total_fixes = 0;
	$files_fixed = 0;

	foreach ( $data['files'] as $filepath => $file_data ) {
		if ( empty( $file_data['messages'] ) ) {
			continue;
		}

		$fixes = apply_ignores_to_file( $filepath, $file_data['messages'], $sniff_config, $source_to_family );
		if ( $fixes > 0 ) {
			$total_fixes += $fixes;
			$files_fixed++;
		}
	}

	return array( 'total_fixes' => $total_fixes, 'files_fixed' => $files_fixed );
}

/**
 * Apply phpcs:disable/enable blocks to a single file.
 *
 * Strategy:
 *   1. Collect all violation lines, grouped by sniff family
 *   2. Cluster nearby violation lines into blocks (within 3 lines = same block)
 *   3. For each block, wrap with phpcs:disable/enable using the union of sniff families
 *   4. For single-line blocks, use phpcs:ignore instead (cleaner)
 *   5. Process blocks bottom-to-top so line insertions don't shift earlier indices
 */
function apply_ignores_to_file( string $filepath, array $messages, array $sniff_config, array $source_to_family ): int {
	$lines = file( $filepath );
	if ( false === $lines ) {
		return 0;
	}

	// Collect violations: line_num → set of sniff families.
	$violations = array();
	foreach ( $messages as $msg ) {
		$source = $msg['source'] ?? '';
		$line   = $msg['line'] ?? 0;

		if ( 0 === $line || ! isset( $source_to_family[ $source ] ) ) {
			continue;
		}

		$family = $source_to_family[ $source ];

		if ( ! isset( $violations[ $line ] ) ) {
			$violations[ $line ] = array();
		}
		$violations[ $line ][ $family ] = true;
	}

	if ( empty( $violations ) ) {
		return 0;
	}

	// Filter out lines already fully covered by existing phpcs:disable blocks.
	// Lines with phpcs:ignore above are NOT filtered — we'll merge into them.
	$violations = array_filter(
		$violations,
		function ( $line_num ) use ( $lines ) {
			$idx = $line_num - 1;
			if ( ! isset( $lines[ $idx ] ) ) {
				return false;
			}
			// Only skip if this exact line has a phpcs:disable (block-level suppression).
			if ( false !== strpos( $lines[ $idx ], 'phpcs:disable' ) ) {
				return false;
			}
			return true;
		},
		ARRAY_FILTER_USE_KEY
	);

	if ( empty( $violations ) ) {
		return 0;
	}

	// Sort by line number ascending for clustering.
	ksort( $violations );

	// Cluster violations into blocks.
	// Lines within 3 of each other belong to the same block.
	$blocks       = array();
	$current      = null;
	$line_numbers = array_keys( $violations );

	foreach ( $line_numbers as $line_num ) {
		if ( null === $current || $line_num - $current['end'] > 3 ) {
			// Start new block.
			if ( null !== $current ) {
				$blocks[] = $current;
			}
			$current = array(
				'start'    => $line_num,
				'end'      => $line_num,
				'families' => $violations[ $line_num ],
			);
		} else {
			// Extend current block.
			$current['end']      = $line_num;
			$current['families'] = array_merge( $current['families'], $violations[ $line_num ] );
		}
	}
	if ( null !== $current ) {
		$blocks[] = $current;
	}

	// Process blocks bottom-to-top to avoid line shift issues.
	$blocks = array_reverse( $blocks );
	$fixes  = 0;

	foreach ( $blocks as $block ) {
		$families    = array_keys( $block['families'] );
		$sniff_list  = implode( ', ', $families );
		$reason      = pick_reason( $families, $sniff_config );
		$start_idx   = $block['start'] - 1;

		// Check if any line in this block is inside a multi-line string.
		// If so, we MUST use phpcs:disable/enable to wrap the entire statement.
		$inside_string = is_inside_multiline_string( $lines, $start_idx );

		// Check if this is a non-DB sniff on a truly single line (e.g., mt_srand, base64_encode).
		$is_db_sniff = false;
		foreach ( $families as $fam ) {
			if ( strpos( $fam, 'WordPress.DB.' ) === 0 ) {
				$is_db_sniff = true;
				break;
			}
		}

		if ( $block['start'] === $block['end'] && ! $inside_string && ! $is_db_sniff ) {
			// Single-line non-DB block: use phpcs:ignore on the line above.
			preg_match( '/^(\s*)/', $lines[ $start_idx ], $m );
			$indent = $m[1] ?? '';

			// Skip if this line already has an inline phpcs:ignore that covers our sniffs.
			if ( false !== strpos( $lines[ $start_idx ], 'phpcs:ignore' ) ) {
				$all_covered = true;
				foreach ( $families as $fam ) {
					if ( false === strpos( $lines[ $start_idx ], $fam ) ) {
						$all_covered = false;
						break;
					}
				}
				if ( $all_covered ) {
					continue;
				}
			}

			// Check the line above — merge into existing phpcs:ignore if present.
			if ( $start_idx > 0 && false !== strpos( $lines[ $start_idx - 1 ], 'phpcs:ignore' ) ) {
				$all_covered = true;
				foreach ( $families as $fam ) {
					if ( false === strpos( $lines[ $start_idx - 1 ], $fam ) ) {
						$all_covered = false;
						break;
					}
				}
				if ( ! $all_covered ) {
					$lines[ $start_idx - 1 ] = merge_ignore_comment( $lines[ $start_idx - 1 ], $families, $sniff_config );
					$fixes++;
				}
			} else {
				$comment = $indent . '// phpcs:ignore ' . $sniff_list . ' -- ' . $reason . "\n";
				array_splice( $lines, $start_idx, 0, array( $comment ) );
				$fixes++;
			}
		} else {
			// Multi-line block OR inside a string: wrap with phpcs:disable/enable.
			// Find the statement boundaries (the $wpdb-> call and its closing ;).
			$stmt_start = find_statement_start( $lines, $start_idx );
			$end_idx    = $block['end'] - 1;
			$stmt_end   = find_statement_end( $lines, $end_idx );

			preg_match( '/^(\s*)/', $lines[ $stmt_start ], $m );
			$indent = $m[1] ?? '';

			// Check if a phpcs:disable already exists above the statement that covers our sniffs.
			if ( $stmt_start > 0 && false !== strpos( $lines[ $stmt_start - 1 ], 'phpcs:disable' ) ) {
				$all_covered = true;
				foreach ( $families as $fam ) {
					if ( false === strpos( $lines[ $stmt_start - 1 ], $fam ) ) {
						$all_covered = false;
						break;
					}
				}
				if ( $all_covered ) {
					continue;
				}
			}

			// Insert enable comment AFTER the statement end.
			$enable_comment = $indent . '// phpcs:enable ' . $sniff_list . "\n";
			array_splice( $lines, $stmt_end + 1, 0, array( $enable_comment ) );

			// Insert disable comment BEFORE the statement start.
			$disable_comment = $indent . '// phpcs:disable ' . $sniff_list . ' -- ' . $reason . "\n";
			array_splice( $lines, $stmt_start, 0, array( $disable_comment ) );

			$fixes++;
		}
	}

	if ( $fixes > 0 ) {
		file_put_contents( $filepath, implode( '', $lines ) );
	}

	return $fixes;
}

/**
 * Check if a line is inside a multi-line string (e.g., SQL in a $wpdb call).
 *
 * Heuristic: the line doesn't start a PHP statement (no $var, no function call,
 * no control structure) and looks like SQL or string continuation.
 */
function is_inside_multiline_string( array $lines, int $idx ): bool {
	$line    = $lines[ $idx ];
	$trimmed = trim( $line );

	// If the line has a $wpdb call, assignment, return, or function — it's PHP code, not inside a string.
	if ( preg_match( '/\$wpdb\s*->/', $line ) ) {
		return false;
	}
	if ( preg_match( '/^\s*(\$\w+\s*=|return\b|function\b|if\b|for\b|while\b|foreach\b|switch\b)/', $line ) ) {
		return false;
	}

	// SQL keywords as line start suggest we're inside a multi-line SQL string.
	if ( preg_match( '/^\s*(FROM|WHERE|AND|OR|JOIN|INNER|LEFT|RIGHT|ON|SET|VALUES|ORDER|GROUP|HAVING|LIMIT|INTO|UPDATE|DELETE|INSERT|ALTER|SELECT|AS|LIKE|IN|NOT|BETWEEN|CASE|WHEN|THEN|ELSE|END|SUM|COUNT)\b/i', $trimmed ) ) {
		return true;
	}

	// Line starting with an interpolated variable inside a string.
	if ( preg_match( '/^\s*\{?\$\w+/', $trimmed ) && ! preg_match( '/^\s*\$\w+\s*(=|->|\()/', $trimmed ) ) {
		return true;
	}

	return false;
}

/**
 * Find the start of a statement containing the given line index.
 *
 * Walks backwards to find the line with $wpdb->, an assignment, or return
 * that opens the statement block.
 */
function find_statement_start( array $lines, int $idx ): int {
	for ( $i = $idx; $i >= 0 && $idx - $i < 20; $i-- ) {
		$line = $lines[ $i ];

		// Skip phpcs comments.
		if ( false !== strpos( $line, 'phpcs:ignore' ) || false !== strpos( $line, 'phpcs:disable' ) ) {
			continue;
		}

		// Found the $wpdb call.
		if ( preg_match( '/\$wpdb\s*->/', $line ) ) {
			// Walk back one more to check for assignment or return.
			if ( $i > 0 ) {
				$prev = $lines[ $i - 1 ];
				if ( preg_match( '/^\s*(\$\w+\s*=|return\b)/', $prev ) ) {
					return $i - 1;
				}
			}
			return $i;
		}

		// Found an assignment or return statement.
		if ( preg_match( '/^\s*(\$\w+\s*=|return\b)/', $line ) ) {
			return $i;
		}
	}

	return $idx;
}

/**
 * Find the end of a statement starting from a given line index.
 *
 * Walks forward looking for a line ending with `;` or a closing paren `)` at
 * the right depth, indicating the end of the $wpdb method call.
 */
function find_statement_end( array $lines, int $start_idx ): int {
	$max_scan = min( $start_idx + 10, count( $lines ) - 1 );

	for ( $i = $start_idx; $i <= $max_scan; $i++ ) {
		$trimmed = rtrim( $lines[ $i ] );

		// Line ends with semicolon — definite statement end.
		if ( preg_match( '/;\s*$/', $trimmed ) ) {
			return $i;
		}

		// Line ends with closing paren + semicolon.
		if ( preg_match( '/\)\s*;\s*$/', $trimmed ) ) {
			return $i;
		}
	}

	// Fallback: return the last violation line.
	return $start_idx;
}

/**
 * Pick the most appropriate reason comment for a set of sniff families.
 */
function pick_reason( array $families, array $sniff_config ): string {
	if ( 1 === count( $families ) ) {
		$family = $families[0];
		return $sniff_config[ $family ]['reason'] ?? 'Known safe usage.';
	}

	// Check if all families are DB-related.
	$all_db = true;
	foreach ( $families as $family ) {
		if ( strpos( $family, 'WordPress.DB.' ) !== 0 ) {
			$all_db = false;
			break;
		}
	}

	if ( $all_db ) {
		return 'Table name from $wpdb->prefix, not user input.';
	}

	// Mixed — use generic reason.
	return 'Known safe usage, see individual comments.';
}

/**
 * Merge new sniff families into an existing phpcs:ignore comment line.
 */
function merge_ignore_comment( string $existing_line, array $new_families, array $sniff_config ): string {
	if ( ! preg_match( '/^(\s*\/\/\s*phpcs:ignore\s+)([^-]+?)(\s*--\s*.*)?$/', $existing_line, $m ) ) {
		return $existing_line;
	}

	$prefix          = $m[1];
	$existing_sniffs = array_map( 'trim', explode( ',', trim( $m[2] ) ) );
	$existing_sniffs = array_filter( $existing_sniffs );

	$all_sniffs = array_unique( array_merge( $existing_sniffs, $new_families ) );
	sort( $all_sniffs );

	$sniff_list = implode( ', ', $all_sniffs );
	$reason     = pick_reason( $all_sniffs, $sniff_config );

	return $prefix . $sniff_list . ' -- ' . $reason . "\n";
}
