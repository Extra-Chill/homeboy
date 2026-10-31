<?php
/**
 * Plugin Name: WP Config Defines Fixture
 * Description: Smoke fixture for the wp_config_defines setting end-to-end. Components declare extra wp-config-level constants under extensions.wordpress.settings.wp_config_defines in their homeboy.json; the dispatcher merges them via HOMEBOY_SETTINGS_JSON and the runner appends them to wp-tests-config.php during boot.
 *
 * The accompanying tests/bench workload reads back the constants and
 * proves they round-trip with PHP type preservation: a string stays a
 * string, an int stays an int, true stays true.
 *
 * Pair fixture for bench-noop / bench-shared-state — adds the
 * wp_config_defines contract to the canonical bench surface.
 */
