<?php

require_once ABSPATH . 'wp-admin/includes/plugin.php';

if ( ! defined( 'SYNTHETIC_NETWORK_FIXTURE_LOADED' ) || ! is_plugin_active_for_network( 'synthetic-network-fixture/network-fixture.php' ) ) {
	throw new RuntimeException( 'The network-only fixture plugin is not loaded and network active.' );
}

$host = wp_parse_url( network_home_url( '/' ), PHP_URL_HOST );
$user = get_user_by( 'login', 'network_fixture_user' );
if ( ! $user ) {
	throw new RuntimeException( 'The shared network user is missing.' );
}

$observed = array();
foreach ( array( 'alpha', 'beta' ) as $site_key ) {
	$sites = get_sites(
		array(
			'domain' => $host,
			'path'   => '/' . $site_key . '/',
			'number' => 1,
		)
	);
	if ( 1 !== count( $sites ) ) {
		throw new RuntimeException( 'Fixture site lookup was not deterministic for ' . $site_key . '.' );
	}

	$site_id = (int) $sites[0]->blog_id;
	if ( ! is_user_member_of_blog( (int) $user->ID, $site_id ) ) {
		throw new RuntimeException( 'The shared network user is not a member of ' . $site_key . '.' );
	}

	switch_to_blog( $site_id );
	$current_user = get_user_by( 'login', 'network_fixture_user' );
	$page         = get_page_by_path( 'fixture-check', OBJECT, 'page' );
	$observed[]   = array(
		'user_id' => $current_user ? (int) $current_user->ID : 0,
		'option'  => get_option( 'synthetic_site_state' ),
		'page'    => $page ? $page->post_content : '',
		'network' => get_site_option( 'synthetic_network_state' ),
	);
	restore_current_blog();
}

if ( $observed[0]['user_id'] !== $observed[1]['user_id'] || (int) $user->ID !== $observed[0]['user_id'] ) {
	throw new RuntimeException( 'User identity is not shared across sites.' );
}
if ( 'isolated-alpha' !== $observed[0]['option'] || 'isolated-beta' !== $observed[1]['option'] || $observed[0]['page'] === $observed[1]['page'] ) {
	throw new RuntimeException( 'Site-scoped option or content isolation failed.' );
}
if ( 'shared-network-value' !== $observed[0]['network'] || $observed[0]['network'] !== $observed[1]['network'] ) {
	throw new RuntimeException( 'Network-scoped state is not shared.' );
}
