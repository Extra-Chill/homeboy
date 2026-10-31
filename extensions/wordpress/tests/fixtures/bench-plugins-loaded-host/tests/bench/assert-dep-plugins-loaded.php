<?php
/**
 * Workload that proves the validation-dep's `plugins_loaded` callback fired.
 *
 * The fixture dep (bench-plugins-loaded-dep) sets
 * $GLOBALS['homeboy_bench_426_dep_plugins_loaded_fired'] inside a
 * `plugins_loaded` callback. If the bench runner loads the dep AFTER
 * `plugins_loaded` has already fired (the bug fixed by homeboy-extensions#426),
 * the global stays unset and this workload throws — which the smoke test
 * surfaces as a STAGE_FAIL on `run_workloads`.
 *
 * Returns a metric so the smoke can also assert the value via the
 * BenchResults envelope without parsing the stage log.
 */
return function (): array {
    $fired = !empty($GLOBALS['homeboy_bench_426_dep_plugins_loaded_fired']);
    if (!$fired) {
        throw new RuntimeException(
            'Dep plugins_loaded callback did not fire — bench runner loaded the dep too late. '
            . 'See homeboy-extensions#426.'
        );
    }

    // wp-phpunit's install path runs wp-settings.php under wp_installing(),
    // which short-circuits the lazy abilities registry init. Fire both
    // canonical actions once so plugin-declared categories and abilities land
    // in the registry before we resolve. (Same pattern used by the bench
    // runner's `ability` step type.)
    if (function_exists('did_action') && function_exists('do_action')) {
        if (!did_action('wp_abilities_api_categories_init')) {
            do_action('wp_abilities_api_categories_init');
        }
        if (!did_action('wp_abilities_api_init')) {
            do_action('wp_abilities_api_init');
        }
    }

    if (!function_exists('is_plugin_active')) {
        require_once ABSPATH . 'wp-admin/includes/plugin.php';
    }

    $plugin_file = 'bench-plugins-loaded-dep/bench-plugins-loaded-dep.php';
    $plugin_active = is_plugin_active($plugin_file);

    $ability_registered = function_exists('wp_get_ability')
        && wp_get_ability('bench-plugins-loaded-dep/ping') !== null;

    return [
        'metrics' => [
            'dep_plugins_loaded_fired' => 1,
            'dep_ability_registered' => $ability_registered ? 1 : 0,
            'dep_plugin_active' => $plugin_active ? 1 : 0,
        ],
        'metadata' => [
            'fixture' => 'bench-plugins-loaded-host',
            'asserts_issue' => 'homeboy-extensions#426',
            'active_plugins' => array_values((array) get_option('active_plugins', [])),
        ],
    ];
};
