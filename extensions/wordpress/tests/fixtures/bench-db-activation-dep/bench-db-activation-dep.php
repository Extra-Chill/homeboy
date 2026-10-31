<?php
/**
 * Plugin Name: Bench DB Activation Dep Fixture
 * Description: Validation-dependency fixture whose activation hook touches
 *              the WordPress database. Used by the bench runner smoke test
 *              for homeboy-extensions#431 to prove plugin activation runs
 *              AFTER wp-phpunit's install path has created the test tables.
 *
 * Behavior:
 *   - Registers an activation hook that calls `add_option()` (touches the
 *     wptests_options table) AND `get_users()` (queries the wptests_users
 *     table). Pre-#431 this fataled with "no such table: wptests_options"
 *     because activation fired during muplugins_loaded — before wp-phpunit's
 *     install.php had created the test schema.
 *   - On `plugins_loaded`, sets a global flag so workloads can verify the
 *     dep entry file was loaded inside the canonical bootstrap window
 *     (homeboy-extensions#426/#427 behavior).
 *
 * The activation callback is intentionally minimal — the point isn't to
 * exercise any specific WordPress API surface but to prove that ANY
 * DB-touching activation hook works under the bench runner. If this
 * fixture's activation runs without fatals against SQLite-backed test
 * tables, real plugins like Data Machine (whose activation hook seeds
 * default options and queries users) will too.
 */

add_action( 'plugins_loaded', static function (): void {
    $GLOBALS['homeboy_bench_431_dep_plugins_loaded_fired'] = true;
} );

register_activation_hook( __FILE__, static function (): void {
    // wptests_options write — pre-#431 this fataled with
    // "no such table: wptests_options".
    add_option(
        'homeboy_bench_431_dep_activated',
        array(
            'fired_at_microtime' => microtime( true ),
            'wp_installing'      => function_exists( 'wp_installing' ) ? wp_installing() : null,
        )
    );

    // wptests_users read — same shape as Data Machine's
    // datamachine_ensure_default_memory_files() activation path that
    // exposed the missing-tables fatal in the original trace.
    if ( function_exists( 'get_users' ) ) {
        $users = get_users( array( 'number' => 1, 'fields' => 'ID' ) );
        update_option(
            'homeboy_bench_431_dep_activation_user_query_count',
            is_array( $users ) ? count( $users ) : 0
        );
    }
} );
