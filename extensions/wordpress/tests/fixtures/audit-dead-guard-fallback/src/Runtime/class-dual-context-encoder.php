<?php
/**
 * Encoder helper that prefers WordPress JSON encoding when available and
 * otherwise falls back to the PHP standard library. The fallback is reached
 * during pure-PHP smoke tests run without WordPress loaded.
 */

final class Dual_Context_Encoder {
	/**
	 * Encode a value as JSON.
	 *
	 * @param mixed $value Value to encode.
	 * @return string|false
	 */
	public static function encode( $value ) {
		if ( function_exists( 'wp_json_encode' ) ) {
			return wp_json_encode( $value );
		}

		// phpcs:ignore WordPress.WP.AlternativeFunctions.json_encode_json_encode -- Pure-PHP smoke tests run without WordPress loaded.
		return json_encode( $value );
	}
}
