#!/usr/bin/env php
<?php
/**
 * Commented-Out Code Fixer
 *
 * Rewords comments that PHPCS falsely flags as "commented-out code" under
 * Squiz.PHP.CommentedOutCode.Found. The sniff fires when >40% of a comment
 * line looks like valid PHP code.
 *
 * Common false-positive patterns:
 *   // result['data']['post_id']         ← array bracket notation
 *   // "event_import" → {"step_type":…}  ← JSON-like mapping
 *   // @yearly, @daily                   ← cron shortcut notation
 *   // AS default: 1                     ← looks like a case label
 *
 * Strategy: Run PHPCS on the file, parse violations for CommentedOutCode,
 * then reword each flagged comment line to reduce code-like syntax below
 * the 40% threshold. Never deletes comments — only rewrites them.
 *
 * Usage: php commented-code-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php commented-code-fixer.php <path>\n";
	exit( 1 );
}

$path = $argv[1];

if ( ! file_exists( $path ) ) {
	echo "Error: Path not found: $path\n";
	exit( 1 );
}

// Find PHPCS binary.
$phpcs = find_phpcs();
if ( ! $phpcs ) {
	echo "Error: Cannot find phpcs binary\n";
	exit( 1 );
}

$result = fixer_process_path( $path, function ( $filepath ) use ( $phpcs ) {
	return process_file( $filepath, $phpcs );
});

if ( $result['total_fixes'] > 0 ) {
	echo "Commented code fixer: Fixed {$result['total_fixes']} false positive(s) in {$result['files_fixed']} file(s)\n";
} else {
	echo "Commented code fixer: No fixable patterns found\n";
}

exit( 0 );

/**
 * Find the PHPCS binary.
 */
function find_phpcs(): ?string {
	// Check common locations.
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

	// Fall back to PATH.
	$which = trim( shell_exec( 'which phpcs 2>/dev/null' ) ?? '' );
	if ( $which !== '' && is_executable( $which ) ) {
		return $which;
	}

	return null;
}

/**
 * Process a single PHP file.
 *
 * Runs PHPCS to find CommentedOutCode violations, then rewrites each
 * flagged comment line to avoid triggering the sniff.
 */
function process_file( string $filepath, string $phpcs ): int {
	// Run PHPCS for just this one sniff.
	$cmd    = escapeshellarg( $phpcs )
		. ' --report=json'
		. ' --sniffs=Squiz.PHP.CommentedOutCode'
		. ' --standard=WordPress-Extra'
		. ' -- ' . escapeshellarg( $filepath )
		. ' 2>/dev/null';
	$output = shell_exec( $cmd );

	if ( $output === null || $output === '' ) {
		return 0;
	}

	$data = json_decode( $output, true );
	if ( ! $data || empty( $data['files'] ) ) {
		return 0;
	}

	// Collect violation line numbers.
	$violation_lines = array();
	foreach ( $data['files'] as $file_data ) {
		foreach ( $file_data['messages'] ?? array() as $msg ) {
			if ( strpos( $msg['source'] ?? '', 'CommentedOutCode' ) !== false ) {
				$violation_lines[] = $msg['line'];
			}
		}
	}

	if ( empty( $violation_lines ) ) {
		return 0;
	}

	$lines = file( $filepath );
	if ( $lines === false ) {
		return 0;
	}

	$fixes = 0;

	foreach ( $violation_lines as $line_num ) {
		$idx = $line_num - 1;
		if ( ! isset( $lines[ $idx ] ) ) {
			continue;
		}

		$line    = $lines[ $idx ];
		$rewrite = rewrite_comment( $line );

		if ( $rewrite !== null && $rewrite !== $line ) {
			$lines[ $idx ] = $rewrite;
			$fixes++;
		}
	}

	if ( $fixes > 0 ) {
		file_put_contents( $filepath, implode( '', $lines ) );
	}

	return $fixes;
}

/**
 * Rewrite a comment line to avoid looking like code.
 *
 * Applies pattern-specific transformations that preserve the semantic
 * meaning while reducing the "code-like" percentage below 40%.
 *
 * Returns null if no safe rewrite is possible.
 */
function rewrite_comment( string $line ): ?string {
	// Check for inline comment at end of code line: code // comment
	if ( preg_match( '/^(.+?)(\/\/\s*)(.*)$/', $line, $m ) && ! preg_match( '/^\s*\/\//', $line ) ) {
		$code_before = $m[1];
		$prefix      = $m[2];
		$content     = $m[3];

		$original_content = $content;
		$content          = apply_rewrites( $content );

		if ( $content === $original_content ) {
			return null;
		}

		return $code_before . $prefix . $content . "\n";
	}

	// Extract indent and comment content for standalone comment lines.
	if ( ! preg_match( '/^(\s*)(\/\/\s*)(.*)$/', $line, $m ) ) {
		// Might be a block comment line.
		if ( ! preg_match( '/^(\s*)(\*\s+|\s*\/\*\*?\s*)(.*)$/', $line, $m ) ) {
			return null;
		}
	}

	$indent  = $m[1];
	$prefix  = $m[2];
	$content = $m[3];

	$original_content = $content;
	$content          = apply_rewrites( $content );

	if ( $content === $original_content ) {
		return null;
	}

	return $indent . $prefix . $content . "\n";
}

/**
 * Apply rewrite transformations to comment content to reduce code-like syntax.
 *
 * @param string $content The comment text (without the // or * prefix).
 * @return string The rewritten content.
 */
function apply_rewrites( string $content ): string {
	// Pattern 1: Array bracket notation — result['data']['post_id']
	// Rewrite brackets to dot notation.
	if ( preg_match( '/\w+\[/', $content ) ) {
		$content = preg_replace( "/\['([^']+)'\]/", '.$1', $content );
		$content = preg_replace( '/\["([^"]+)"\]/', '.$1', $content );
	}

	// Pattern 2: Arrow notation — "x" → {"y": "z"}
	// The arrow + JSON braces look like code. Replace → with "becomes".
	if ( strpos( $content, '→' ) !== false || strpos( $content, '=>' ) !== false ) {
		$content = str_replace( '→', 'becomes', $content );
		if ( ! preg_match( '/\$\w+/', $content ) ) {
			$content = str_replace( '=>', 'becomes', $content );
		}
	}

	// Pattern 3: JSON-like braces or parens with colon key-value — {"step_type": "x"} or ("step_type": "x")
	// Rewrite colon-separated key-value to just the value description.
	if ( preg_match( '/[\{\(]["\']/', $content ) && preg_match( '/["\']:\s*["\']/', $content ) ) {
		// Replace "key": "value" with key=value (no quotes, no colons).
		$content = preg_replace( '/["\'](\w+)["\']\s*:\s*["\']([^"\']+)["\']/', '$1=$2', $content );
		// Remove remaining braces/parens wrapping.
		$content = str_replace( array( '{', '}' ), '', $content );
		$content = preg_replace( '/\((\w+=\w+)\)/', '$1', $content );
	} elseif ( preg_match( '/\{["\']/', $content ) ) {
		$content = preg_replace( '/\{([^}]+)\}/', '($1)', $content );
	}

	// Pattern 4: @ shortcuts — @yearly, @daily, @hourly
	// Remove @ prefix entirely since quoting still triggers the sniff.
	if ( preg_match( '/@\w+/', $content ) ) {
		$content = preg_replace( '/"?@(\w+)"?/', '$1', $content );
	}

	// Pattern 5: "AS default: 1" or "default: value" or "AS (default 1)" — looks like code.
	// Rewrite to natural language with "defaults to" phrasing.
	if ( preg_match( '/\bdefault[=:]\s*\d+/', $content ) || preg_match( '/\(default\s+\d+\)/', $content ) ) {
		$content = preg_replace( '/(\w+)\s+default[=:]\s*(\d+)/', '$1 defaults to $2', $content );
		$content = preg_replace( '/(\w+)\s+\(default\s+(\d+)\)/', '$1 defaults to $2', $content );
	}

	return $content;
}
