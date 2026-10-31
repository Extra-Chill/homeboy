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
	'ajaxActions'  => array(),
	'cronEvents'   => array(),
	'cronSchedules' => array(),
	'postTypes'    => array(),
	'taxonomies'   => array(),
	'roles'        => array(),
	'capabilities' => array(),
	'users'        => array(),
	'options'      => array(),
	'settings'     => array(),
	'media'        => array(),
	'wpCliCommands' => array(),
	'unsupported'  => array(),
);

$artifact['restRoutes'] = homeboy_wordpress_live_discover_rest_routes( $artifact );
$artifact['adminPages'] = homeboy_wordpress_live_discover_admin_pages( $artifact );
$artifact['databaseTables'] = homeboy_wordpress_live_discover_database_tables( $artifact );
$artifact['frontendUrls'] = homeboy_wordpress_live_discover_frontend_urls( $artifact );
$artifact['blocks'] = homeboy_wordpress_live_discover_blocks( $artifact );
$artifact['ajaxActions'] = homeboy_wordpress_live_discover_ajax_actions( $artifact );
$artifact['cronEvents'] = homeboy_wordpress_live_discover_cron_events( $artifact );
$artifact['cronSchedules'] = homeboy_wordpress_live_discover_cron_schedules( $artifact );
$artifact['postTypes'] = homeboy_wordpress_live_discover_post_types( $artifact );
$artifact['taxonomies'] = homeboy_wordpress_live_discover_taxonomies( $artifact );
$artifact['roles'] = homeboy_wordpress_live_discover_roles( $artifact );
$artifact['capabilities'] = homeboy_wordpress_live_discover_capabilities( $artifact );
$artifact['users'] = homeboy_wordpress_live_discover_users( $artifact );
$artifact['options'] = homeboy_wordpress_live_discover_options( $artifact );
$artifact['settings'] = homeboy_wordpress_live_discover_settings( $artifact );
$artifact['media'] = homeboy_wordpress_live_discover_media( $artifact );
$artifact['wpCliCommands'] = homeboy_wordpress_live_discover_wp_cli_commands( $artifact );

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

function homeboy_wordpress_live_discover_ajax_actions( array &$artifact ) {
	global $wp_filter;

	if ( ! is_array( $wp_filter ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'ajax_action', 'missing_hook_registry', '$wp_filter is unavailable.' );
		return array();
	}

	$actions = array();
	foreach ( array_keys( $wp_filter ) as $hook ) {
		if ( 0 === strpos( $hook, 'wp_ajax_' ) ) {
			$actions[] = array(
				'action'        => substr( $hook, strlen( 'wp_ajax_' ) ),
				'authenticated' => true,
				'source'        => 'wp_filter',
			);
			continue;
		}
		if ( 0 === strpos( $hook, 'wp_ajax_nopriv_' ) ) {
			$actions[] = array(
				'action'        => substr( $hook, strlen( 'wp_ajax_nopriv_' ) ),
				'authenticated' => false,
				'source'        => 'wp_filter',
			);
		}
	}

	return $actions;
}

function homeboy_wordpress_live_discover_cron_events( array &$artifact ) {
	if ( ! function_exists( '_get_cron_array' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'cron_event', 'missing_cron_api', '_get_cron_array() is unavailable.' );
		return array();
	}

	$events = array();
	foreach ( (array) _get_cron_array() as $timestamp => $cronhooks ) {
		foreach ( (array) $cronhooks as $hook => $instances ) {
			foreach ( (array) $instances as $instance ) {
				$events[] = array(
					'event'     => $hook,
					'timestamp' => (int) $timestamp,
					'schedule'  => is_array( $instance ) && isset( $instance['schedule'] ) ? (string) $instance['schedule'] : '',
					'source'    => '_get_cron_array',
				);
			}
		}
	}

	return $events;
}

function homeboy_wordpress_live_discover_cron_schedules( array &$artifact ) {
	if ( ! function_exists( 'wp_get_schedules' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'cron_event', 'missing_cron_schedules_api', 'wp_get_schedules() is unavailable.' );
		return array();
	}

	$schedules = array();
	foreach ( wp_get_schedules() as $name => $schedule ) {
		$schedules[] = array(
			'event'    => $name,
			'label'    => isset( $schedule['display'] ) ? (string) $schedule['display'] : $name,
			'interval' => isset( $schedule['interval'] ) ? (int) $schedule['interval'] : null,
			'source'   => 'wp_get_schedules',
		);
	}

	return $schedules;
}

function homeboy_wordpress_live_discover_post_types( array &$artifact ) {
	if ( ! function_exists( 'get_post_types' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'post_type', 'missing_post_type_api', 'get_post_types() is unavailable.' );
		return array();
	}

	$post_types = array();
	foreach ( get_post_types( array(), 'objects' ) as $name => $post_type ) {
		$post_types[] = array(
			'name'       => $name,
			'label'      => $post_type->label ?? $name,
			'public'     => ! empty( $post_type->public ),
			'showUi'     => ! empty( $post_type->show_ui ),
			'showInRest' => ! empty( $post_type->show_in_rest ),
			'restBase'   => $post_type->rest_base ?? '',
			'source'     => 'get_post_types',
		);
	}

	return $post_types;
}

function homeboy_wordpress_live_discover_taxonomies( array &$artifact ) {
	if ( ! function_exists( 'get_taxonomies' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'taxonomy', 'missing_taxonomy_api', 'get_taxonomies() is unavailable.' );
		return array();
	}

	$taxonomies = array();
	foreach ( get_taxonomies( array(), 'objects' ) as $name => $taxonomy ) {
		$taxonomies[] = array(
			'name'       => $name,
			'label'      => $taxonomy->label ?? $name,
			'public'     => ! empty( $taxonomy->public ),
			'hierarchical' => ! empty( $taxonomy->hierarchical ),
			'showInRest' => ! empty( $taxonomy->show_in_rest ),
			'objectTypes' => array_values( (array) ( $taxonomy->object_type ?? array() ) ),
			'restBase'   => $taxonomy->rest_base ?? '',
			'source'     => 'get_taxonomies',
		);
	}

	return $taxonomies;
}

function homeboy_wordpress_live_discover_roles( array &$artifact ) {
	if ( ! function_exists( 'wp_roles' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'role', 'missing_roles_api', 'wp_roles() is unavailable.' );
		return array();
	}

	$roles = array();
	foreach ( wp_roles()->roles as $role => $definition ) {
		$roles[] = array(
			'role'            => $role,
			'label'           => $definition['name'] ?? $role,
			'capabilityCount' => isset( $definition['capabilities'] ) && is_array( $definition['capabilities'] ) ? count( $definition['capabilities'] ) : 0,
			'source'          => 'wp_roles',
		);
	}

	return $roles;
}

function homeboy_wordpress_live_discover_capabilities( array &$artifact ) {
	if ( ! function_exists( 'wp_roles' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'capability', 'missing_roles_api', 'wp_roles() is unavailable.' );
		return array();
	}

	$capabilities = array();
	foreach ( wp_roles()->roles as $role => $definition ) {
		foreach ( array_keys( (array) ( $definition['capabilities'] ?? array() ) ) as $capability ) {
			$capabilities[ $capability ][] = $role;
		}
	}

	ksort( $capabilities );
	return array_map(
		function ( $capability, $roles ) {
			return array(
				'capability' => $capability,
				'roleCount'   => count( array_unique( $roles ) ),
				'source'      => 'wp_roles',
			);
		},
		array_keys( $capabilities ),
		array_values( $capabilities )
	);
}

function homeboy_wordpress_live_discover_users( array &$artifact ) {
	if ( ! function_exists( 'count_users' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'user', 'missing_user_api', 'count_users() is unavailable.' );
		return array();
	}

	$counts = count_users();
	$users  = array(
		array(
			'user'    => 'all',
			'label'   => 'All users',
			'count'   => isset( $counts['total_users'] ) ? (int) $counts['total_users'] : 0,
			'source'  => 'count_users',
		),
	);
	foreach ( (array) ( $counts['avail_roles'] ?? array() ) as $role => $count ) {
		$users[] = array(
			'user'   => 'role:' . $role,
			'label'  => 'Users with role ' . $role,
			'count'  => (int) $count,
			'source' => 'count_users',
		);
	}

	return $users;
}

function homeboy_wordpress_live_discover_options( array &$artifact ) {
	global $wpdb;

	if ( ! isset( $wpdb ) || ! method_exists( $wpdb, 'get_results' ) || empty( $wpdb->options ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'option', 'missing_wpdb', '$wpdb options table access is unavailable.' );
		return array();
	}

	$rows = $wpdb->get_results( "SELECT option_name, autoload FROM {$wpdb->options} ORDER BY option_name", ARRAY_A );
	if ( ! is_array( $rows ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'option', 'options_unavailable', 'Options metadata query returned no rows.' );
		return array();
	}

	return array_map(
		function ( $row ) {
			return array(
				'option'   => $row['option_name'] ?? '',
				'autoload' => $row['autoload'] ?? '',
				'source'   => 'wpdb_options_metadata',
			);
		},
		$rows
	);
}

function homeboy_wordpress_live_discover_settings( array &$artifact ) {
	global $wp_registered_settings;

	if ( ! is_array( $wp_registered_settings ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'setting', 'missing_settings_registry', '$wp_registered_settings is unavailable.' );
		return array();
	}

	$settings = array();
	foreach ( $wp_registered_settings as $name => $setting ) {
		$settings[] = array(
			'setting' => $name,
			'settingType' => isset( $setting['type'] ) ? (string) $setting['type'] : '',
			'group'   => isset( $setting['group'] ) ? (string) $setting['group'] : '',
			'showInRest' => ! empty( $setting['show_in_rest'] ),
			'source'  => 'wp_registered_settings',
		);
	}

	return $settings;
}

function homeboy_wordpress_live_discover_media( array &$artifact ) {
	if ( ! function_exists( 'wp_count_attachments' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'media', 'missing_media_api', 'wp_count_attachments() is unavailable.' );
		return array();
	}

	$counts = wp_count_attachments();
	$media  = array();
	foreach ( get_object_vars( $counts ) as $mime_type => $count ) {
		$media[] = array(
			'media'  => $mime_type,
			'label'  => $mime_type,
			'count'  => (int) $count,
			'source' => 'wp_count_attachments',
		);
	}

	return $media;
}

function homeboy_wordpress_live_discover_wp_cli_commands( array &$artifact ) {
	if ( ! class_exists( 'WP_CLI' ) || ! method_exists( 'WP_CLI', 'get_root_command' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'wp_cli_command', 'missing_wp_cli_runtime', 'WP_CLI command registry is unavailable.' );
		return array();
	}

	$root = WP_CLI::get_root_command();
	if ( ! is_object( $root ) || ! method_exists( $root, 'get_subcommands' ) ) {
		homeboy_wordpress_live_unsupported( $artifact, 'wp_cli_command', 'missing_wp_cli_command_registry', 'WP_CLI root command registry is unavailable.' );
		return array();
	}

	return homeboy_wordpress_live_collect_wp_cli_subcommands( $root, array() );
}

function homeboy_wordpress_live_collect_wp_cli_subcommands( $command, array $prefix ) {
	$commands = array();
	foreach ( $command->get_subcommands() as $name => $subcommand ) {
		$path       = array_merge( $prefix, array( $name ) );
		$commands[] = array(
			'command' => implode( ' ', $path ),
			'source'  => 'WP_CLI_command_registry',
		);
		if ( is_object( $subcommand ) && method_exists( $subcommand, 'get_subcommands' ) ) {
			$commands = array_merge( $commands, homeboy_wordpress_live_collect_wp_cli_subcommands( $subcommand, $path ) );
		}
	}

	return $commands;
}
