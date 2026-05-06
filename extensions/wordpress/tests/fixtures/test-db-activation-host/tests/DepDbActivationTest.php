<?php
/**
 * PHPUnit-side regression guard for homeboy-extensions#431.
 *
 * The bench and test runners had to be patched to defer plugin activation
 * until AFTER wp-phpunit's install.php creates the wptests_* tables. This
 * test pins that behavior on the test runner side: a future refactor that
 * fires `activate_<plugin>` inline during muplugins_loaded (the pre-#431
 * shape) would make the dep's activation hook fatal trying to write
 * wptests_options or query wptests_users.
 *
 * Runs via: wordpress/scripts/test/playground-db-activation-smoke.sh
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

class DepDbActivationTest extends WP_UnitTestCase {

    /**
     * The fixture dep (bench-db-activation-dep) writes a structured option
     * inside its activation callback. The option being readable here proves
     * activation ran AFTER wp-phpunit's install path created
     * wptests_options. Pre-#431 this option was never written because the
     * activation hook fataled with "no such table: wptests_options".
     */
    public function test_dep_activation_option_was_written() {
        $marker = get_option( 'homeboy_bench_431_dep_activated', false );
        $this->assertIsArray(
            $marker,
            'Dep activation hook did not write its option — homeboy-extensions#431 regression. '
            . 'Activation fired before wp-phpunit created wptests_options.'
        );
        $this->assertArrayHasKey( 'fired_at_microtime', $marker );
        $this->assertArrayHasKey( 'wp_installing', $marker );
    }

    /**
     * The fixture dep also runs `get_users()` inside its activation
     * callback and stores the row count. The option being readable proves
     * the wptests_users table existed when activation fired — the exact
     * shape Data Machine's activation path needed in the original trace.
     */
    public function test_dep_activation_get_users_query_succeeded() {
        $count = get_option( 'homeboy_bench_431_dep_activation_user_query_count', null );
        $this->assertIsNumeric(
            $count,
            'Dep activation hook did not record a get_users() query result — '
            . 'wptests_users was not available when activation fired.'
        );
        $this->assertGreaterThanOrEqual(
            0,
            (int) $count,
            'get_users() returned a non-numeric count, suggesting the call fataled or short-circuited.'
        );
    }

    /**
     * Sanity check: the dep's plugins_loaded callback must still fire,
     * proving the load-ordering fix from #426/#427 is preserved alongside
     * the activation-timing fix from #431. Don't lose what #426/#427 won.
     */
    public function test_dep_plugins_loaded_callback_still_fires() {
        $this->assertTrue(
            ! empty( $GLOBALS['homeboy_bench_431_dep_plugins_loaded_fired'] ),
            'Validation dep plugins_loaded callback did not fire — homeboy-extensions#426/#427 regressed.'
        );
    }
}
