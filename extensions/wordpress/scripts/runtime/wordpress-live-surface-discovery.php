<?php
/**
 * Emit live WordPress runtime surfaces for Homeboy discovery.
 *
 * Intended usage inside a booted WordPress/Codebox runtime:
 * wp eval-file wordpress/scripts/runtime/wordpress-live-surface-discovery.php
 */

$artifact = array(
	'schema'       => 'homeboy/wordpress-live-surface-discovery-raw/v1',
	'generated_at' => gmdate( 'c' ),
	'source'       => 'wp-cli-eval-file',
	'restRoutes'   => array(),
	'adminPages'   => array(),
	'databaseTables' => array(),
	'frontendUrls' => array(),
	'blocks'       => array(),
	'unsupported'  => array(),
);

$artifact['restRoutes'] = homeboy_wordpress_live_discover_rest_routes( $artifact );
$artifact['adminPages'] = homeboy_wordpress_live_discover_admin_pages( $artifact );
$artifact['databaseTables'] = homeboy_wordpress_live_discover_database_tables( $artifact );
$artifact['frontendUrls'] = homeboy_wordpress_live_discover_frontend_urls( $artifact );
$artifact['blocks'] = homeboy_wordpress_live_discover_blocks( $artifact );

echo wp_json_encode( $artifact, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES ) . PHP_EOL;

function homeboy_wordpress_live_unsupported( array &$artifact, $type, $reason, $message ) {
	$artifact['unsupported'][] = array(
		'type'    => $type,
		'reason'  => $reason,
		'message' => $message,
	);
}

function homeboy_wordpress_live_discover_rest_routes( array &$artifact ) {
	if ( ! function_exists( 'rest_get_server' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'rest_route', 'missing_rest_server', 'rest_get_server() is unavailable.' );
		return array();
	}

	$routes = array();
	foreach ( rest_get_server()->get_routes() as $route => $handlers ) {
		$methods = array();
		foreach ( (array) $handlers as $handler ) {
			if ( isset( $handler['methods'] ) && is_array( $handler['methods'] ) ) {
				$methods = array_merge( $methods, array_keys( $handler['methods'] ) );
			}
		}
		$routes[] = array(
			'route'   => $route,
			'path'    => preg_replace( '/\(\?P<([^>]+)>[^)]+\)/', '{$1}', $route ),
			'methods' => array_values( array_unique( array_filter( $methods ) ) ),
			'source'  => 'rest_get_server',
		);
	}

	return $routes;
}

function homeboy_wordpress_live_discover_admin_pages( array &$artifact ) {
	global $menu, $submenu;

	if ( ! is_admin() && ! defined( 'WP_ADMIN' ) ) {
		define( 'WP_ADMIN', true );
	}

	try {
		do_action( 'admin_menu', '' );
	} catch ( Throwable $error ) {
		homeboy_wordpress_live_unsupported( $artifact, 'admin_page', 'admin_menu_failed', $error->getMessage() );
		return array();
	}

	$pages = array();
	foreach ( (array) $menu as $item ) {
		if ( empty( $item[2] ) ) {
			continue;
		}
		$pages[] = array(
			'path'   => homeboy_wordpress_live_admin_path( $item[2] ),
			'name'   => wp_strip_all_tags( (string) ( $item[0] ?? $item[2] ) ),
			'source' => 'admin_menu',
		);
	}
	foreach ( (array) $submenu as $parent => $items ) {
		foreach ( (array) $items as $item ) {
			if ( empty( $item[2] ) ) {
				continue;
			}
			$pages[] = array(
				'path'   => homeboy_wordpress_live_admin_path( $item[2] ),
				'name'   => wp_strip_all_tags( (string) ( $item[0] ?? $item[2] ) ),
				'parent' => $parent,
				'source' => 'admin_menu',
			);
		}
	}

	return $pages;
}

function homeboy_wordpress_live_admin_path( $slug ) {
	return '/wp-admin/' . ltrim( (string) $slug, '/' );
}

function homeboy_wordpress_live_discover_database_tables( array &$artifact ) {
	global $wpdb;

	if ( ! isset( $wpdb ) || ! method_exists( $wpdb, 'get_results' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'db_table', 'missing_wpdb', '$wpdb is unavailable.' );
		return array();
	}

	$rows = $wpdb->get_results( 'SHOW TABLE STATUS', ARRAY_A );
	if ( ! is_array( $rows ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'db_table', 'table_status_unavailable', 'SHOW TABLE STATUS returned no rows.' );
		return array();
	}

	return array_map(
		function ( $row ) {
			return array(
				'name'       => $row['Name'] ?? '',
				'engine'     => $row['Engine'] ?? '',
				'rowCount'   => isset( $row['Rows'] ) ? (int) $row['Rows'] : 0,
				'dataBytes'  => isset( $row['Data_length'] ) ? (int) $row['Data_length'] : 0,
				'indexBytes' => isset( $row['Index_length'] ) ? (int) $row['Index_length'] : 0,
				'source'     => 'wpdb',
			);
		},
		$rows
	);
}

function homeboy_wordpress_live_discover_frontend_urls( array &$artifact ) {
	if ( ! function_exists( 'home_url' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'frontend_url', 'missing_home_url', 'home_url() is unavailable.' );
		return array();
	}

	$urls = array(
		array(
			'url'    => home_url( '/' ),
			'label'  => 'Home',
			'source' => 'home_url',
		),
	);

	$front_page_id = (int) get_option( 'page_on_front' );
	if ( $front_page_id > 0 ) {
		$urls[] = array(
			'url'    => get_permalink( $front_page_id ),
			'label'  => 'Static front page',
			'source' => 'page_on_front',
		);
	}

	return $urls;
}

function homeboy_wordpress_live_discover_blocks( array &$artifact ) {
	if ( ! class_exists( 'WP_Block_Type_Registry' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'block', 'missing_block_registry', 'WP_Block_Type_Registry is unavailable.' );
		return array();
	}

	$blocks = array();
	foreach ( WP_Block_Type_Registry::get_instance()->get_all_registered() as $name => $block_type ) {
		$blocks[] = array(
			'name'       => $name,
			'title'      => $block_type->title ?? '',
			'category'   => $block_type->category ?? '',
			'attributes' => is_array( $block_type->attributes ?? null ) ? array_keys( $block_type->attributes ) : array(),
			'source'     => 'WP_Block_Type_Registry',
		);
	}

	return $blocks;
}
