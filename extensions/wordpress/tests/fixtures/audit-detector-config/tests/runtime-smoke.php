<?php
// Pure-PHP smoke tests run without WordPress loaded.
if ( ! function_exists( 'wp_json_encode' ) ) {
	function wp_json_encode( $value ) {
		return json_encode( $value );
	}
}
