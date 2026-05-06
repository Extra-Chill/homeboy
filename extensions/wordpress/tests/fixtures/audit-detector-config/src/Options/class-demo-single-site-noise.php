<?php
/**
 * False-positive fixture for the wordpress-option-scope-drift detector.
 *
 * This file mentions "multisite" and "network" only in passing — discussing
 * an incidental network request, a multisite-aware logger that lives in
 * another package, and the fact that this plugin is intentionally
 * single-site. None of the comments here make a claim about how any
 * option below is supposed to be stored.
 *
 * The detector MUST NOT fire on this file. Under the previous rule, every
 * `(get|update|delete)_option` call below would have been flagged because
 * the words "multisite" and "network" appear above. The tightened
 * `comment_pattern` ignores these incidental mentions.
 */

class Demo_Single_Site_Noise {

	/**
	 * Read the plugin's normal single-site option.
	 *
	 * The retry logic below makes a network request to refresh the cache
	 * before reading. The multisite-aware logger lives in another package
	 * and is not used here.
	 */
	public function read() {
		return get_option( 'demo_single_site_setting' );
	}

	public function write( $value ) {
		return update_option( 'demo_single_site_setting', $value );
	}

	public function clear() {
		return delete_option( 'demo_single_site_setting' );
	}
}
