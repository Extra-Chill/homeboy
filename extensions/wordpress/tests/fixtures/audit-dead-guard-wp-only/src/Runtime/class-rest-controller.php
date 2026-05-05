<?php
/**
 * REST controller that defensively guards genuinely-WordPress-only symbols.
 * Both guards are dead at runtime: the plugin floor guarantees the symbols
 * whenever WordPress is loaded, and neither symbol has a meaningful pure-PHP
 * fallback shape, so dead_guard should still report them.
 */

final class REST_Controller {
	public static function boot(): void {
		if ( ! class_exists( 'WP_REST_Server' ) ) {
			return;
		}

		if ( function_exists( 'register_rest_route' ) ) {
			register_rest_route( 'demo/v1', '/ping', array() );
		}
	}
}
