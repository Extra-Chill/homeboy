<?php
/**
 * File-level opt-out fixture for the wordpress-option-scope-drift detector.
 *
 * @option-scope single-site
 *
 * This file may legitimately discuss network-scoped storage in a comment for
 * documentation purposes — for example, explaining that the plugin's storage
 * is stored as a site option in a hypothetical multisite future. The
 * `@option-scope single-site` marker above tells the detector that this file
 * has already declared its intended option scope, so the comment-pattern
 * trigger should be suppressed even though "stored as a site option" appears
 * below.
 */

class Demo_Opt_Out_Marker {

	/**
	 * Hypothetically this would be stored as a site option in a multisite
	 * deployment, but this plugin is intentionally single-site only and the
	 * `@option-scope single-site` marker above documents that decision.
	 */
	public function read() {
		return get_option( 'demo_opt_out_setting' );
	}

	public function write( $value ) {
		return update_option( 'demo_opt_out_setting', $value );
	}

	public function clear() {
		return delete_option( 'demo_opt_out_setting' );
	}
}
