<?php
/**
 * Plugin Name: Synthetic Network Fixture
 * Description: Generic path-based multisite fixture markers.
 * Network: true
 */

define( 'SYNTHETIC_NETWORK_FIXTURE_LOADED', true );

add_action(
	'wp_footer',
	static function () {
		$site = get_option( 'synthetic_site_key', 'main' );
		printf(
			'<div id="synthetic-network-fixture" data-site="%1$s" data-user="%2$d"><span id="synthetic-site-%1$s">%1$s</span><span id="synthetic-auth-%3$s">%3$s</span></div>',
			esc_attr( $site ),
			get_current_user_id(),
			is_user_logged_in() ? 'authenticated' : 'anonymous'
		);
	}
);
