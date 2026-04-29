<?php
/**
 * Fixture test selected by HOMEBOY_CHANGED_TEST_FILES.
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

class SchemaScopeRunsTest extends WP_UnitTestCase {

    public function test_activation_schema_exists() {
        global $wpdb;

        $this->assertSame( 'yes', get_option( 'homeboy_playground_schema_scope_activated' ) );
        $this->assertSame(
            $wpdb->prefix . 'playground_schema_scope',
            $wpdb->get_var( "SELECT name FROM sqlite_master WHERE type='table' AND name='{$wpdb->prefix}playground_schema_scope'" )
        );
    }
}
