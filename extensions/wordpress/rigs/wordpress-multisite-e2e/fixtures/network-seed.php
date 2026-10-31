<?php

require_once ABSPATH . 'wp-admin/includes/plugin.php';

if ( ! is_multisite() ) {
	throw new RuntimeException( 'Expected a multisite runtime.' );
}

$_SERVER['REMOTE_ADDR'] = $_SERVER['REMOTE_ADDR'] ?? '127.0.0.1';

$plugin = 'synthetic-network-fixture/network-fixture.php';
if ( ! is_plugin_active_for_network( $plugin ) ) {
	$result = activate_plugin( $plugin, '', true );
	if ( is_wp_error( $result ) ) {
		throw new RuntimeException( $result->get_error_message() );
	}
}

$host = wp_parse_url( network_home_url( '/' ), PHP_URL_HOST );
if ( ! is_string( $host ) || '' === $host ) {
	throw new RuntimeException( 'Unable to resolve the network host.' );
}

$user = get_user_by( 'login', 'network_fixture_user' );
if ( ! $user ) {
	$user_id = wp_create_user( 'network_fixture_user', 'fixture-password', 'network-fixture@example.test' );
	if ( is_wp_error( $user_id ) ) {
		throw new RuntimeException( $user_id->get_error_message() );
	}
	$user = get_user_by( 'id', $user_id );
}

update_site_option( 'synthetic_network_state', 'shared-network-value' );

$sites = array( 'alpha', 'beta' );
foreach ( $sites as $site_key ) {
	$path     = '/' . $site_key . '/';
	$existing = get_sites(
		array(
			'domain' => $host,
			'path'   => $path,
			'number' => 1,
		)
	);
	$site_id  = $existing ? (int) $existing[0]->blog_id : wpmu_create_blog( $host, $path, ucfirst( $site_key ) . ' Fixture Site', (int) $user->ID );

	if ( is_wp_error( $site_id ) || ! $site_id ) {
		throw new RuntimeException( 'Unable to create fixture site ' . $site_key . '.' );
	}

	add_user_to_blog( $site_id, (int) $user->ID, 'administrator' );
	switch_to_blog( $site_id );
	update_option( 'synthetic_site_key', $site_key );
	update_option( 'synthetic_site_state', 'isolated-' . $site_key );
	update_option( 'permalink_structure', '/%postname%/' );
	$page = get_page_by_path( 'fixture-check', OBJECT, 'page' );
	if ( ! $page ) {
		$page_id = wp_insert_post(
			array(
				'post_title'   => ucfirst( $site_key ) . ' Fixture Check',
				'post_name'    => 'fixture-check',
				'post_content' => 'Synthetic content for ' . $site_key . '.',
				'post_status'  => 'publish',
				'post_type'    => 'page',
				'post_author'  => (int) $user->ID,
			),
			true
		);
		if ( is_wp_error( $page_id ) ) {
			restore_current_blog();
			throw new RuntimeException( $page_id->get_error_message() );
		}
	}
	flush_rewrite_rules();
	restore_current_blog();
}

update_option( 'synthetic_site_key', 'main' );
update_option( 'synthetic_site_state', 'isolated-main' );
