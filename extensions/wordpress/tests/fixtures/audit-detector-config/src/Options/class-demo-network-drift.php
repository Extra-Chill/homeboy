<?php
/**
 * True-positive fixture for the wordpress-option-scope-drift detector.
 *
 * The docblock explicitly claims this value is stored as a network option and
 * should use update_site_option / get_site_option, but the implementation
 * calls the single-site `update_option` family. The detector MUST flag every
 * `(get|update|delete)_option` call site in this file.
 */

class Demo_Network_Drift {

	/**
	 * Read the network-wide setting.
	 *
	 * This setting is shared across the network and stored as a network
	 * option, so callers should use get_site_option here.
	 */
	public function read() {
		return get_option( 'demo_network_setting' );
	}

	/**
	 * Persist the network-wide option.
	 *
	 * Network-scoped storage: must use update_site_option.
	 */
	public function write( $value ) {
		return update_option( 'demo_network_setting', $value );
	}

	/**
	 * Delete the network-wide option.
	 *
	 * Stored as a site option — should use delete_site_option.
	 */
	public function clear() {
		return delete_option( 'demo_network_setting' );
	}
}
