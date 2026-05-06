<?php
/**
 * Smoke coverage for Playground plugin activation and changed-test scoping.
 */

$result_file = tempnam( sys_get_temp_dir(), 'pg-schema-scope-' );
$current_stage = 'preboot';
$assertions = 0;
$actions = array();
$options = array();
$current_hook = null;
$wp_installing = true;

function assert_true_smoke( $condition, $message ) {
    global $assertions;
    ++$assertions;
    if ( ! $condition ) {
        throw new RuntimeException( $message );
    }
}

function add_action( $hook, $callback ) {
    global $actions;
    $actions[ $hook ][] = $callback;
}

function do_action( $hook, ...$args ) {
    global $actions, $current_hook;
    $previous_hook = $current_hook;
    $current_hook = $hook;
    foreach ( $actions[ $hook ] ?? array() as $callback ) {
        call_user_func_array( $callback, $args );
    }
    $current_hook = $previous_hook;
}

function current_filter() {
    return $GLOBALS['current_hook'];
}

function wp_installing() {
    return $GLOBALS['wp_installing'];
}

function plugin_basename( $file ) {
    return basename( dirname( $file ) ) . '/' . basename( $file );
}

function register_activation_hook( $file, $callback ) {
    add_action( 'activate_' . plugin_basename( $file ), $callback );
}

function update_option( $name, $value ) {
    global $options;
    $options[ $name ] = $value;
}

class Smoke_WPDB {
    public $prefix = 'wptests_';
    public $queries = array();

    public function query( $sql ) {
        $this->queries[] = $sql;
        return true;
    }
}

$wpdb = new Smoke_WPDB();

require_once dirname( __DIR__ ) . '/lib/playground-bootstrap.php';

$fixture_path = dirname( __DIR__, 2 ) . '/tests/fixtures/playground-schema-scope';
$component_file = pg_run_load_component_stage( array( 'plugin_path' => $fixture_path, 'activate' => false ) );

$install_load_log = file_get_contents( $result_file );
assert_true_smoke(
    strpos( $install_load_log, 'PLUGIN_LOAD_CONTEXT playground-schema-scope.php activate=false stage=load_component hook=none wp_installing=true' ) !== false,
    'Install-time plugin load context was not logged.'
);
assert_true_smoke(
    strpos( $install_load_log, 'PLUGIN_ACTIVATE_BEGIN' ) === false,
    'Activation diagnostics should not be logged when activation is disabled during install-time load.'
);
assert_true_smoke(
    is_string( $component_file ) && basename( $component_file ) === 'playground-schema-scope.php',
    'pg_run_load_component_stage should return the discovered plugin entry file path.'
);

// homeboy-extensions#431 split: load_component only requires the file. Activation
// is dispatched separately through pg_run_activation_stage() AFTER install creates
// the test tables. Re-call load_component to confirm `'activate' => true` no longer
// fires activation inline (post-#431 the key is a no-op kept for back-compat), then
// drive activation through the new stage.
$GLOBALS['wp_installing'] = false;
$component_file_post_install = pg_run_load_component_stage( array( 'plugin_path' => $fixture_path ) );
assert_true_smoke(
    is_string( $component_file_post_install ) && basename( $component_file_post_install ) === 'playground-schema-scope.php',
    'pg_run_load_component_stage should return the entry file path on the post-install call as well.'
);

$post_load_log = file_get_contents( $result_file );
assert_true_smoke(
    strpos( $post_load_log, 'PLUGIN_LOAD_CONTEXT playground-schema-scope.php activate=true stage=load_component hook=none wp_installing=false' ) !== false,
    'Post-install plugin load context was not logged.'
);
assert_true_smoke(
    substr_count( $post_load_log, 'PLUGIN_ACTIVATE_BEGIN' ) === 0,
    'pg_run_load_component_stage must not fire activation hooks inline post-#431; that is pg_run_activation_stage()\'s job.'
);

pg_run_activation_stage( array( 'plugin_files' => array( $component_file_post_install ) ) );

$activation_log = file_get_contents( $result_file );
assert_true_smoke(
    strpos( $activation_log, 'PLUGIN_ACTIVATE_BEGIN playground-schema-scope/playground-schema-scope.php stage=activation hook=none wp_installing=false' ) !== false,
    'Post-install activation begin context was not logged from pg_run_activation_stage().'
);
assert_true_smoke(
    strpos( $activation_log, 'PLUGIN_ACTIVATE_OK playground-schema-scope/playground-schema-scope.php stage=activation hook=none wp_installing=false' ) !== false,
    'Post-install activation completion context was not logged from pg_run_activation_stage().'
);
assert_true_smoke(
    strpos( $activation_log, 'STAGE_OK:activation' ) !== false,
    'pg_run_activation_stage() did not emit STAGE_OK:activation on success.'
);
assert_true_smoke(
    substr_count( $activation_log, 'PLUGIN_ACTIVATE_BEGIN' ) === 1,
    'Activation should fire exactly once per plugin file passed to pg_run_activation_stage().'
);

assert_true_smoke(
    isset( $GLOBALS['options']['homeboy_playground_schema_scope_activated'] ),
    'Plugin activation hook did not update its activation option.'
);
assert_true_smoke(
    count( $GLOBALS['wpdb']->queries ) === 1,
    'Plugin activation hook did not run the schema query exactly once.'
);
assert_true_smoke(
    strpos( $GLOBALS['wpdb']->queries[0], 'CREATE TABLE wptests_playground_schema_scope' ) !== false,
    'Plugin activation hook did not create the expected prefixed table.'
);

$all_test_files = array(
    $fixture_path . '/tests/SchemaScopeRunsTest.php',
    $fixture_path . '/tests/UnselectedScopeTest.php',
);
$filtered = pg_filter_changed_test_files(
    $all_test_files,
    json_encode( array( 'tests/SchemaScopeRunsTest.php' ) ),
    $fixture_path
);

assert_true_smoke( count( $filtered ) === 1, 'Changed-test scope did not reduce the file list to one test.' );
assert_true_smoke(
    basename( $filtered[0] ) === 'SchemaScopeRunsTest.php',
    'Changed-test scope kept the wrong test file.'
);

$vfs_filtered = pg_filter_changed_test_files(
    array( '/wordpress/wp-content/plugins/example/tests/SchemaScopeRunsTest.php' ),
    json_encode( array( '/Users/example/plugin/tests/SchemaScopeRunsTest.php' ) ),
    '/wordpress/wp-content/plugins/example'
);
assert_true_smoke( count( $vfs_filtered ) === 1, 'Changed-test scope did not normalize absolute host paths.' );

@unlink( $result_file );
echo "Playground schema/scope smoke passed ($assertions assertions)\n";
