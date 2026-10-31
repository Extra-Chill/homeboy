<?php
/**
 * Integration test: custom db.php drop-in coexists with Playground's built-in
 * SQLite integration.
 *
 * Runs via the WordPress extension WP Codebox test runner.
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

class DropInCoexistenceTest extends WP_UnitTestCase {

    /**
     * The fixture's db.php defines HOMEBOY_DROPIN_FIXTURE_LOADED. If this
     * constant is missing, WordPress never actually loaded the drop-in —
     * either the file isn't at /wp-content/db.php or Playground's internal
     * SQLite mu-plugin stopped stepping aside.
     */
    public function test_dropin_was_loaded() {
        $this->assertTrue(
            defined( 'HOMEBOY_DROPIN_FIXTURE_LOADED' ) && HOMEBOY_DROPIN_FIXTURE_LOADED,
            'Plugin-provided db.php drop-in did not execute during bootstrap. '
            . 'Check: (1) db.php mounted at /wordpress/wp-content/db.php, '
            . '(2) Playground mu-plugin guard still intact upstream.'
        );
    }

    /**
     * Proves $wpdb still serves real WordPress writes after the drop-in
     * installed itself. If this regresses, the drop-in hooked itself in
     * before wp-settings completed DB setup (which happens for several
     * misconfigurations of the MDI-style 'reuse internal SQLite' pattern).
     */
    public function test_wpdb_can_insert_and_read() {
        $post_id = wp_insert_post( array(
            'post_title'   => 'Drop-in smoke',
            'post_status'  => 'publish',
            'post_content' => 'Lorem ipsum',
        ) );

        $this->assertIsInt( $post_id );
        $this->assertGreaterThan( 0, $post_id );

        $post = get_post( $post_id );
        $this->assertNotNull( $post );
        $this->assertSame( 'Drop-in smoke', $post->post_title );
        $this->assertSame( 'publish', $post->post_status );
    }

    /**
     * Proves that Playground's internal SQLite mu-plugin voluntarily stepped
     * aside. We verify this indirectly: WP_SQLite_DB is available (so the
     * bundled implementation IS on the VFS) but DATABASE_TYPE was set by our
     * drop-in, not by the mu-plugin's early-exit path.
     *
     * The mu-plugin defines SQLITE_DB_DROPIN_VERSION on its own path. Our
     * drop-in does NOT. So seeing that constant undefined (or defined by the
     * fixture itself if someone extends it) is the signal that the
     * mu-plugin's body never executed.
     */
    public function test_playground_mu_plugin_stepped_aside() {
        // WP_SQLite_DB exists because we loaded it ourselves from the VFS
        // in db.php. That's expected.
        $this->assertTrue( class_exists( 'WP_SQLite_DB' ), 'Bundled SQLite implementation should be reachable from the VFS.' );

        // SQLITE_DB_DROPIN_VERSION is defined at the TOP of the Playground
        // mu-plugin's body. If the mu-plugin's guard fired, the body never
        // ran, so the constant stays undefined.
        $this->assertFalse(
            defined( 'SQLITE_DB_DROPIN_VERSION' ),
            'SQLITE_DB_DROPIN_VERSION is defined, which means Playground\'s '
            . 'SQLite mu-plugin DID execute. The wp-content/db.php guard is '
            . 'not firing — the drop-in would have been silently overwritten.'
        );
    }
}
