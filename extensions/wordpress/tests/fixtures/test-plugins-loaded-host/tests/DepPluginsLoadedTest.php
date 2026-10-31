<?php
/**
 * PHPUnit-side regression guard for homeboy-extensions#426.
 *
 * The bench runner had to be patched to load validation-dependency entry
 * files before wp-settings.php fires `plugins_loaded`. The test runner
 * already loaded deps at the right point (via tests_add_filter on
 * muplugins_loaded). This test pins that behavior so a future refactor of
 * the test runner's boot order can't silently regress validation-dependency
 * lifecycle support.
 *
 * Runs via: wordpress/scripts/test/playground-dep-plugins-loaded-smoke.sh
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

class DepPluginsLoadedTest extends WP_UnitTestCase {

    /**
     * The fixture dep (bench-plugins-loaded-dep) sets a global in its
     * `plugins_loaded` callback. If the test runner ever started loading
     * the dep AFTER plugins_loaded fired, the global would stay unset and
     * this test would fail.
     */
    public function test_dep_plugins_loaded_callback_fired() {
        $this->assertTrue(
            !empty( $GLOBALS['homeboy_bench_426_dep_plugins_loaded_fired'] ),
            'Validation dep plugins_loaded callback did not fire. '
            . 'The test runner must require dep entry files BEFORE wp-settings.php '
            . 'fires plugins_loaded — see homeboy-extensions#426.'
        );
    }

    /**
     * The dep registers an Abilities API callback via the canonical
     * `wp_abilities_api_init` action. If the dep file loaded too late
     * (post-plugins_loaded), `add_action` would still succeed but the
     * resulting callback would never run because `wp_abilities_api_init`
     * had already fired with no listeners.
     *
     * Asserting on the registered-callbacks list (not the actual ability)
     * keeps this test backend-agnostic — wp-phpunit's install path unhooks
     * a few core ability registrations and runs under wp_installing(), so
     * the actual ability resolution path is fragile in this environment.
     * The registration itself is the contract we care about for #426.
     */
    public function test_dep_added_wp_abilities_api_init_callback() {
        global $wp_filter;
        $this->assertArrayHasKey(
            'wp_abilities_api_init',
            $wp_filter,
            'No wp_abilities_api_init callbacks registered — the dep file was '
            . 'never loaded, or loaded so late that its add_action() call ran '
            . 'against a hook that had already been pruned.'
        );
        $this->assertNotEmpty(
            $wp_filter['wp_abilities_api_init']->callbacks,
            'wp_abilities_api_init has no callbacks registered. The dep file '
            . 'was loaded too late for its add_action() to take effect — see '
            . 'homeboy-extensions#426.'
        );
    }
}
