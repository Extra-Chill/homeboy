<?php
/**
 * Smoke coverage for Playground plugin activation and changed-test scoping.
 */

$result_file = tempnam( sys_get_temp_dir(), 'pg-schema-scope-' );
$current_stage = 'preboot';
$assertions = 0;
$actions = array();
$options = array();

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
    global $actions;
    foreach ( $actions[ $hook ] ?? array() as $callback ) {
        call_user_func_array( $callback, $args );
    }
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
pg_run_load_component_stage( array( 'plugin_path' => $fixture_path ) );

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
