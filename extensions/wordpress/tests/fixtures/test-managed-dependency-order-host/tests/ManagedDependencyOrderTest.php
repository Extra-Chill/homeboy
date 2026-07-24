<?php

class ManagedDependencyOrderTest extends WP_UnitTestCase {
    public function test_dependency_loads_after_install_before_component_lifecycle(): void {
        $this->assertFalse( $GLOBALS['homeboy_managed_dependency_available_during_install'] );
        $this->assertFalse( $GLOBALS['homeboy_managed_dependency_class_preloaded'] );
        $this->assertTrue( $GLOBALS['homeboy_managed_dependency_loaded'] );
        $this->assertSame(
            'managed-dependency-loaded',
            $GLOBALS['homeboy_managed_dependency_component_value']
        );
        $this->assertSame(
            'managed-dependency-loaded',
            get_option( 'homeboy_managed_dependency_activated' )
        );
    }

    public function test_managed_install_preserves_wordpress_database(): void {
        global $wpdb;

        $this->assertInstanceOf( wpdb::class, $wpdb );
        $this->assertSame( '1', (string) $wpdb->get_var( 'SELECT 1' ) );
    }
}
