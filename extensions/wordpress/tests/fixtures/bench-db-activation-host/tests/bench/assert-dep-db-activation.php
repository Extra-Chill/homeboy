<?php
/**
 * Workload that proves the validation-dep's DB-touching activation hook fired
 * cleanly against the wp-phpunit-installed test schema (homeboy-extensions#431).
 *
 * The fixture dep (bench-db-activation-dep) registers an activation hook that
 * calls `add_option()` (writes to wptests_options) AND `get_users()` (reads
 * from wptests_users). Pre-#431 the bench runner fired activation inline
 * during muplugins_loaded — before wp-phpunit's install.php created those
 * tables — and the dep activation fataled with "no such table: wptests_*".
 *
 * Post-#431 the runner splits load and activation: load_deps inside
 * muplugins_loaded only require_once's the entry file; pg_run_activation_stage
 * fires the activation hook AFTER pg_run_install_stage returns, so the
 * tables exist by the time `add_option()` and `get_users()` run.
 *
 * If this workload sees the dep's plugins_loaded flag set AND the activation
 * option present, both the load-ordering fix (#426/#427) AND the activation-
 * timing fix (#431) are working.
 */
return function (): array {
    $loaded_flag = !empty( $GLOBALS['homeboy_bench_431_dep_plugins_loaded_fired'] );
    if ( ! $loaded_flag ) {
        throw new RuntimeException(
            'Dep plugins_loaded callback did not fire — bench runner regressed homeboy-extensions#426/#427.'
        );
    }

    $activation_marker = function_exists( 'get_option' )
        ? get_option( 'homeboy_bench_431_dep_activated', false )
        : false;
    if ( ! is_array( $activation_marker ) ) {
        throw new RuntimeException(
            'Dep activation hook did not write its option — homeboy-extensions#431 fix is missing or broken. '
            . 'The activation hook should fire AFTER wp-phpunit creates wptests_options.'
        );
    }

    $user_query_count = function_exists( 'get_option' )
        ? get_option( 'homeboy_bench_431_dep_activation_user_query_count', null )
        : null;
    if ( ! is_numeric( $user_query_count ) ) {
        throw new RuntimeException(
            'Dep activation hook did not record a wptests_users query result — '
            . 'get_users() likely fataled because activation ran before install.php created the table.'
        );
    }

    return array(
        'metrics'  => array(
            'dep_plugins_loaded_fired'           => 1,
            'dep_activation_option_set'          => 1,
            'dep_activation_user_query_succeeded' => 1,
        ),
        'metadata' => array(
            'fixture'                  => 'bench-db-activation-host',
            'asserts_issue'            => 'homeboy-extensions#431',
            'wp_installing_during_act' => $activation_marker['wp_installing'] ?? null,
        ),
    );
};
