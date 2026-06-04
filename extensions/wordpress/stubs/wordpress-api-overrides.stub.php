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
