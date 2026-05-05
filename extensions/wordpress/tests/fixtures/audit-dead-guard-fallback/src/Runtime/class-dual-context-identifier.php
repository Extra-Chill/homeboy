<?php
/**
 * Identifier helper that prefers wp_generate_uuid4 when WordPress is loaded
 * and otherwise falls back to a pure-PHP UUID-shaped random string.
 */

final class Dual_Context_Identifier {
	/**
	 * Generate a request/chain identifier.
	 *
	 * @return string
	 */
	public static function generate(): string {
		if ( function_exists( 'wp_generate_uuid4' ) ) {
			return wp_generate_uuid4();
		}

		return bin2hex( random_bytes( 16 ) );
	}
}
