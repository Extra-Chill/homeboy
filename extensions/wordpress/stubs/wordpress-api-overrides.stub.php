<?php

/**
 * Homeboy's WordPress PHPStan profile models the APIs most commonly used by
 * plugins. These overrides cover gaps in upstream generated stubs without
 * changing runtime behaviour.
 */

// WordPress plugins commonly start with an ABSPATH guard. Without this constant
// in PHPStan's symbol graph, those files look like they unconditionally exit.
define( 'ABSPATH', '/' );

class WP_CLI {
	/**
	 * @param mixed $message
	 * @return void
	 * @phpstan-impure
	 */
	public static function log( $message ) {}

	/**
	 * @param mixed $message
	 * @return void
	 * @phpstan-impure
	 */
	public static function success( $message ) {}

	/**
	 * @param mixed $message
	 * @return void
	 * @phpstan-impure
	 */
	public static function warning( $message ) {}

	/**
	 * @param mixed $message
	 * @param bool|int $exit
	 * @return null
	 * @phpstan-impure
	 */
	public static function error( $message, $exit = true ) {}
}

/**
 * @param non-empty-string $hook_name
 * @param mixed $value
 * @param mixed ...$args
 * @return mixed
 */
function apply_filters( $hook_name, $value, ...$args ) {}

/**
 * @param non-empty-string $hook_name
 * @param callable $callback
 * @return true
 */
function add_filter( $hook_name, $callback, int $priority = 10, int $accepted_args = 1 ) {}

/**
 * @param non-empty-string $hook_name
 * @param callable $callback
 * @return true
 */
function add_action( $hook_name, $callback, int $priority = 10, int $accepted_args = 1 ) {}

/**
 * @param non-empty-string $hook_name
 * @param callable $callback
 */
function remove_filter( $hook_name, $callback, int $priority = 10 ): bool {}

/**
 * @param string $capability
 * @param mixed ...$args
 */
function current_user_can( $capability, ...$args ): bool {}

/**
 * @param mixed $value
 * @return string|false
 */
function wp_json_encode( $value, int $flags = 0, int $depth = 512 ) {}

/**
 * @param int|WP_Post|null $post
 * @param 'OBJECT'|'ARRAY_A'|'ARRAY_N' $output
 * @param 'raw'|'edit'|'db'|'display' $filter
 * @return WP_Post|array<string|int, mixed>|null
 */
function get_post( $post = null, $output = OBJECT, $filter = 'raw' ) {}

/**
 * Stateful reads that PHPStan otherwise treats as pure.
 *
 * Each of these consults the database or the current request, so a repeated
 * call can legitimately return a different value. Without the marker PHPStan
 * folds the second call into the first and reports the guard around it as
 * always-true or always-false — a false positive on correct code.
 *
 * Carried here rather than per component: the annotation is a property of the
 * core function, so every consumer needs it. extrachill-users had declared
 * these three in its own stub file, which is the duplication this replaces.
 */

/** @phpstan-impure */
function metadata_exists( string $meta_type, int $object_id, string $meta_key ): bool {}

/**
 * @phpstan-impure
 * @return WP_User|false
 */
function get_userdata( int $user_id ) {}

/** @phpstan-impure */
function is_user_member_of_blog( int $user_id = 0, int $blog_id = 0 ): bool {}
