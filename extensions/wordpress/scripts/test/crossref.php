#!/usr/bin/env php
<?php
/**
 * WordPress cross-reference extraction for homeboy test --crossref.
 *
 * Reads JSON command from stdin, extracts hook registrations, hook definitions,
 * mock expectations, and method calls from PHP source files. Returns structured
 * JSON on stdout matching homeboy's CrossRefExtraction schema.
 *
 * Protocol:
 *   stdin:  {"command": "extract_crossref", "file": "relative/path.php", "content": "..."}
 *   stdout: {"hook_registrations": [...], "hook_definitions": [...],
 *            "mock_expectations": [...], "method_calls": [...]}
 *
 * WordPress-aware filtering:
 *   - Skips WP core dynamic hooks (pre_option_*, pre_http_request, etc.)
 *   - Skips WP core function calls (get_option, update_option, wp_*, etc.)
 *   - Resolves mock class names from createMock/getMockBuilder context
 */

// ============================================================================
// WordPress core hooks that tests legitimately register for (not plugin bugs).
// These are dynamic hooks fired by WordPress core functions.
// ============================================================================

/**
 * Patterns for WordPress core dynamic hooks.
 * Tests that register for these are intercepting WP core behavior, not testing
 * plugin hooks — they should NOT be flagged as "hook not found in production".
 */
$wp_core_hook_patterns = array(
	// get_option() fires pre_option_{name} before DB lookup
	'/^pre_option_/',
	// update_option() fires pre_update_option_{name}
	'/^pre_update_option_/',
	// get_site_option() fires pre_site_option_{name}
	'/^pre_site_option_/',
	// wp_remote_*() fires pre_http_request to short-circuit HTTP
	'/^pre_http_request$/',
	// transient hooks
	'/^pre_set_transient_/',
	'/^pre_transient_/',
	'/^pre_set_site_transient_/',
	'/^pre_site_transient_/',
	// WP scheduling
	'/^pre_schedule_event$/',
	'/^pre_unschedule_event$/',
	'/^pre_clear_scheduled_hook$/',
	'/^pre_get_scheduled_event$/',
	// WP mail
	'/^pre_wp_mail$/',
);

/**
 * Exact WordPress core hook names that tests commonly register for.
 */
$wp_core_hooks_exact = array(
	// Core lifecycle
	'init',
	'plugins_loaded',
	'admin_init',
	'wp_loaded',
	'shutdown',
	'admin_menu',
	'admin_enqueue_scripts',
	'wp_enqueue_scripts',
	'rest_api_init',
	'wp_ajax_nopriv_',
	// Content
	'the_content',
	'the_title',
	'wp_insert_post',
	'save_post',
	'delete_post',
	'wp_trash_post',
	'transition_post_status',
	'wp_after_insert_post',
	'add_attachment',
	'edit_attachment',
	'delete_attachment',
	'wp_update_attachment_metadata',
	'wp_generate_attachment_metadata',
	// Options
	'added_option',
	'updated_option',
	'deleted_option',
	// HTTP
	'http_response',
	'http_request_args',
	'http_api_debug',
	// Auth
	'auth_cookie_valid',
	'set_auth_cookie',
	'clear_auth_cookie',
	'wp_login',
	'wp_logout',
	// Cron
	'cron_schedules',
	// Meta
	'get_post_metadata',
	'update_post_metadata',
	'add_post_metadata',
	'delete_post_metadata',
	'get_user_metadata',
	'update_user_metadata',
	// REST
	'rest_pre_dispatch',
	'rest_post_dispatch',
	'rest_request_before_callbacks',
	'rest_request_after_callbacks',
	// Query
	'pre_get_posts',
	'posts_clauses',
	'posts_where',
	'posts_join',
	'posts_orderby',
	// User
	'user_register',
	'profile_update',
	'delete_user',
	// Taxonomy
	'created_term',
	'edited_term',
	'delete_term',
	'set_object_terms',
	// Upload
	'upload_mimes',
	'wp_handle_upload',
	'wp_handle_upload_prefilter',
	// Admin
	'admin_notices',
);

/**
 * WordPress core functions — method calls on these should NOT be flagged
 * as mock mismatches.
 */
$wp_core_functions = array(
	// These are global WP functions, not object methods, so they won't
	// appear as method calls. But static calls like WP_Query::get_posts()
	// or WP_REST_Server::dispatch() are WP-internal.
	'__construct',
	'setUp',
	'tearDown',
	'setUpBeforeClass',
	'tearDownBeforeClass',
	// PHPUnit internals
	'expects',
	'method',
	'willReturn',
	'willReturnMap',
	'willReturnCallback',
	'willThrowException',
	'with',
	'withConsecutive',
	'once',
	'never',
	'any',
	'exactly',
	'atLeast',
	'atLeastOnce',
	'atMost',
	'returnValue',
	'returnSelf',
	'returnArgument',
	'returnValueMap',
	'throwException',
	'onConsecutiveCalls',
	'createMock',
	'getMockBuilder',
	'getMock',
	'setMethods',
	'disableOriginalConstructor',
);

// ============================================================================
// Main
// ============================================================================

$input = file_get_contents( 'php://stdin' );
$command = json_decode( $input, true );

if ( ! $command || ! isset( $command['command'] ) || $command['command'] !== 'extract_crossref' ) {
	fwrite( STDERR, "Unknown command\n" );
	exit( 1 );
}

$file    = $command['file'] ?? '';
$content = $command['content'] ?? '';

if ( empty( $content ) ) {
	echo json_encode( array(
		'hook_registrations' => array(),
		'hook_definitions'   => array(),
		'mock_expectations'  => array(),
		'method_calls'       => array(),
	) );
	exit( 0 );
}

$is_test = ( strpos( $file, '/tests/' ) !== false || strpos( $file, 'Test.php' ) !== false );
$lines   = explode( "\n", $content );
$result  = array(
	'hook_registrations' => array(),
	'hook_definitions'   => array(),
	'mock_expectations'  => array(),
	'method_calls'       => array(),
);

// Track mock variable → class mappings for the file.
$mock_class_map = array();

// First pass: build mock class map from createMock/getMockBuilder calls.
foreach ( $lines as $line_num => $line ) {
	$trimmed = trim( $line );

	// $this->mockFoo = $this->createMock( Foo::class );
	// $mockFoo = $this->createMock( \Namespace\Foo::class );
	if ( preg_match( '/\$(?:this->)?(\w+)\s*=\s*\$this->createMock\(\s*([\\\\?\w]+)::class\s*\)/', $trimmed, $m ) ) {
		$var_name  = $m[1];
		$class_fqn = $m[2];
		// Extract short class name.
		$parts     = explode( '\\', $class_fqn );
		$short     = end( $parts );
		$mock_class_map[ $var_name ] = $short;
	}

	// $mockFoo = $this->getMockBuilder( Foo::class )->...->getMock();
	if ( preg_match( '/\$(?:this->)?(\w+)\s*=\s*\$this->getMockBuilder\(\s*([\\\\?\w]+)::class\s*\)/', $trimmed, $m ) ) {
		$var_name  = $m[1];
		$class_fqn = $m[2];
		$parts     = explode( '\\', $class_fqn );
		$short     = end( $parts );
		$mock_class_map[ $var_name ] = $short;
	}
}

// ============================================================================
// Phase 2a: Multi-line hook extraction (content-level regex).
// Handles apply_filters(\n\t'hook_name', ...) and do_action(\n\t'hook_name', ...).
// ============================================================================

// Production: apply_filters/do_action (may span multiple lines).
if ( ! $is_test ) {
	if ( preg_match_all(
		'/\b(apply_filters|do_action)\s*\(\s*[\'"]([^\'"]+)[\'"]/s',
		$content,
		$hook_matches,
		PREG_OFFSET_CAPTURE | PREG_SET_ORDER
	) ) {
		foreach ( $hook_matches as $m ) {
			$func_name  = $m[1][0];
			$hook_name  = $m[2][0];
			$byte_offset = $m[0][1];
			$line_1     = substr_count( $content, "\n", 0, $byte_offset ) + 1;
			$args_count = count_hook_args_from_offset( $content, $m[0][1] + strlen( $m[0][0] ) );

			$result['hook_definitions'][] = array(
				'name'       => $hook_name,
				'file'       => $file,
				'line'       => $line_1,
				'args_count' => $args_count,
				'kind'       => 'definition',
			);
		}
	}
}

// Test: add_filter/add_action/remove_filter/remove_action (may span multiple lines).
if ( $is_test ) {
	if ( preg_match_all(
		'/\b(add_filter|add_action|remove_filter|remove_action)\s*\(\s*[\'"]([^\'"]+)[\'"]/s',
		$content,
		$hook_matches,
		PREG_OFFSET_CAPTURE | PREG_SET_ORDER
	) ) {
		foreach ( $hook_matches as $m ) {
			$func      = $m[1][0];
			$hook_name = $m[2][0];
			$byte_offset = $m[0][1];
			$line_1    = substr_count( $content, "\n", 0, $byte_offset ) + 1;

			// Skip WordPress core hooks.
			if ( is_wp_core_hook( $hook_name, $wp_core_hook_patterns, $wp_core_hooks_exact ) ) {
				continue;
			}

			$args_count = null;
			if ( $func === 'add_filter' ) {
				// For add_filter, arg_count is the 4th argument.
				$match_line = $lines[ $line_1 - 1 ] ?? '';
				$args_count = extract_arg_count( $match_line, $func );
			}

			$result['hook_registrations'][] = array(
				'name'       => $hook_name,
				'file'       => $file,
				'line'       => $line_1,
				'args_count' => $args_count,
				'kind'       => 'registration',
			);
		}
	}
}

// ============================================================================
// Phase 2b: Per-line extraction for mocks and method calls.
// ============================================================================

foreach ( $lines as $line_num => $line ) {
	$trimmed  = trim( $line );
	$line_1   = $line_num + 1; // 1-indexed.

	// Skip comments.
	if ( preg_match( '/^\s*(?:\/\/|#|\*|\/\*)/', $trimmed ) ) {
		continue;
	}

	if ( $is_test ) {
		// Mock expectations: ->method('methodName')
		// Match: ->expects(...)->method('name') or ->method('name')
		if ( preg_match_all( '/->method\(\s*[\'"](\w+)[\'"]\s*\)/', $trimmed, $matches, PREG_SET_ORDER ) ) {
			foreach ( $matches as $m ) {
				$method_name = $m[1];

				// Skip PHPUnit internals.
				if ( in_array( $method_name, $wp_core_functions, true ) ) {
					continue;
				}

				// Try to resolve mock class from variable context.
				$class = resolve_mock_class( $trimmed, $mock_class_map );

				$result['mock_expectations'][] = array(
					'class'  => $class,
					'method' => $method_name,
					'file'   => $file,
					'line'   => $line_1,
				);
			}
		}
	} else {
		// Method calls: $obj->method( or Class::method(
		// Instance calls.
		if ( preg_match_all( '/\$(\w+)->(\w+)\s*\(/', $trimmed, $matches, PREG_SET_ORDER ) ) {
			foreach ( $matches as $m ) {
				$class  = '$' . $m[1];
				$method = $m[2];

				// Skip common noise.
				if ( in_array( $method, $wp_core_functions, true ) ) {
					continue;
				}

				$result['method_calls'][] = array(
					'class'  => $class,
					'method' => $method,
					'file'   => $file,
					'line'   => $line_1,
				);
			}
		}

		// Static calls.
		if ( preg_match_all( '/(\w+)::(\w+)\s*\(/', $trimmed, $matches, PREG_SET_ORDER ) ) {
			foreach ( $matches as $m ) {
				$class  = $m[1];
				$method = $m[2];

				// Skip self/parent/static and common noise.
				if ( in_array( $class, array( 'self', 'parent', 'static' ), true ) ) {
					continue;
				}
				if ( in_array( $method, $wp_core_functions, true ) ) {
					continue;
				}

				$result['method_calls'][] = array(
					'class'  => $class,
					'method' => $method,
					'file'   => $file,
					'line'   => $line_1,
				);
			}
		}
	}
}

echo json_encode( $result );
exit( 0 );

// ============================================================================
// Helper functions
// ============================================================================

/**
 * Check if a hook name is a WordPress core hook.
 *
 * @param string $hook_name Hook name to check.
 * @param array  $patterns  Regex patterns for dynamic WP core hooks.
 * @param array  $exact     Exact WP core hook names.
 * @return bool
 */
function is_wp_core_hook( $hook_name, $patterns, $exact ) {
	// Check exact matches first.
	if ( in_array( $hook_name, $exact, true ) ) {
		return true;
	}

	// Check patterns.
	foreach ( $patterns as $pattern ) {
		if ( preg_match( $pattern, $hook_name ) ) {
			return true;
		}
	}

	return false;
}

/**
 * Extract the arg_count (4th argument) from add_filter() call.
 *
 * @param string $line Line of code.
 * @param string $func Function name (add_filter).
 * @return int|null
 */
function extract_arg_count( $line, $func ) {
	// Find the function call and parse args with depth tracking.
	$pos = strpos( $line, $func );
	if ( $pos === false ) {
		return null;
	}

	$rest = substr( $line, $pos + strlen( $func ) );
	$paren_pos = strpos( $rest, '(' );
	if ( $paren_pos === false ) {
		return null;
	}

	$inner = substr( $rest, $paren_pos + 1 );
	$depth = 0;
	$commas = 0;
	$arg_start = 0;

	for ( $i = 0; $i < strlen( $inner ); $i++ ) {
		$c = $inner[ $i ];
		if ( $c === '(' || $c === '[' ) {
			$depth++;
		} elseif ( $c === ')' || $c === ']' ) {
			if ( $depth === 0 ) {
				break;
			}
			$depth--;
		} elseif ( $c === ',' && $depth === 0 ) {
			$commas++;
			if ( $commas === 3 ) {
				$arg_start = $i + 1;
			}
		}
	}

	if ( $commas >= 3 ) {
		$arg = trim( substr( $inner, $arg_start ) );
		$arg = rtrim( $arg, ')' );
		$arg = trim( $arg );
		if ( is_numeric( $arg ) ) {
			return (int) $arg;
		}
	}

	return null;
}

/**
 * Count arguments passed to apply_filters/do_action (excluding hook name).
 *
 * @param string $line Line of code.
 * @param string $func Function name.
 * @return int
 */
function count_hook_args( $line, $func ) {
	$pos = strpos( $line, $func );
	if ( $pos === false ) {
		return 0;
	}

	$rest = substr( $line, $pos + strlen( $func ) );
	$paren_pos = strpos( $rest, '(' );
	if ( $paren_pos === false ) {
		return 0;
	}

	$inner = substr( $rest, $paren_pos + 1 );
	$depth = 0;
	$commas = 0;

	for ( $i = 0; $i < strlen( $inner ); $i++ ) {
		$c = $inner[ $i ];
		if ( $c === '(' || $c === '[' ) {
			$depth++;
		} elseif ( $c === ')' || $c === ']' ) {
			if ( $depth === 0 ) {
				break;
			}
			$depth--;
		} elseif ( $c === ',' && $depth === 0 ) {
			$commas++;
		}
	}

	// Commas = separators. First arg is hook name. Remaining = commas.
	return $commas;
}

/**
 * Count arguments for apply_filters/do_action from a byte offset in content.
 *
 * Works with multi-line function calls by tracking parenthesis depth from
 * the position after the hook name string argument.
 *
 * @param string $content     Full file content.
 * @param int    $start_offset Byte offset after the hook name string match.
 * @return int Number of arguments after the hook name.
 */
function count_hook_args_from_offset( $content, $start_offset ) {
	$rest  = substr( $content, $start_offset );
	$depth = 0;
	$commas = 0;
	$len   = strlen( $rest );

	// We're positioned after 'hook_name' — skip past the closing quote and comma.
	// Find the first comma (separating hook name from args).
	$started = false;

	for ( $i = 0; $i < $len; $i++ ) {
		$c = $rest[ $i ];

		if ( $c === '(' || $c === '[' ) {
			$depth++;
		} elseif ( $c === ')' || $c === ']' ) {
			if ( $depth === 0 ) {
				break;
			}
			$depth--;
		} elseif ( $c === ',' && $depth === 0 ) {
			$commas++;
		}
	}

	return $commas;
}

/**
 * Try to resolve the mock's class name from the line context.
 *
 * Looks for patterns like: $this->mockFoo->expects(...)->method('bar')
 * and resolves mockFoo → Foo via the class map built from createMock calls.
 *
 * @param string $line      Current line of code.
 * @param array  $class_map Variable name → class name map.
 * @return string Class name or empty string.
 */
function resolve_mock_class( $line, $class_map ) {
	// Pattern: $this->varName->expects or $varName->expects or $this->varName->method
	if ( preg_match( '/\$(?:this->)?(\w+)\s*->\s*(?:expects|method)\b/', $line, $m ) ) {
		$var = $m[1];
		if ( isset( $class_map[ $var ] ) ) {
			return $class_map[ $var ];
		}
	}

	return '';
}
