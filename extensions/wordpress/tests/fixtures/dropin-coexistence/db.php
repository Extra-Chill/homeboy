<?php
/**
 * Plugin Name: Drop-in Coexistence Fixture (db.php)
 *
 * Reference db.php drop-in for the Playground backend's coexistence smoke test.
 *
 * How coexistence works in Playground:
 *
 *   1. Playground ships an internal mu-plugin at
 *      /internal/shared/mu-plugins/sqlite-database-integration.php that
 *      normally wires up $wpdb to its bundled SQLite implementation.
 *
 *   2. That mu-plugin begins with a self-deactivation guard:
 *          if ( file_exists( '/wordpress/wp-content/db.php' ) ) { return; }
 *
 *   3. When we mount a plugin's db.php at /wordpress/wp-content/db.php (the
 *      Playground backend runner does this automatically when it sees a
 *      db.php in the plugin root), the guard fires and the mu-plugin steps
 *      aside. The drop-in then owns $wpdb for the whole request lifecycle.
 *
 *   4. The drop-in is still free to reuse Playground's bundled SQLite
 *      implementation — it lives at /internal/shared/sqlite-database-integration
 *      and is readable at runtime. That's the pattern
 *      markdown-database-integration uses in production.
 *
 * This fixture delegates to Playground's bundled SQLite so the rest of the
 * WordPress test suite (wp_insert_post, wp_get_user, etc.) continues to
 * work end-to-end. A real drop-in (MDI) would substitute its own $wpdb
 * subclass at the end.
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

// Signal that this drop-in ran. The accompanying test class asserts this.
if ( ! defined( 'HOMEBOY_DROPIN_FIXTURE_LOADED' ) ) {
    define( 'HOMEBOY_DROPIN_FIXTURE_LOADED', true );
}

$sqlite = '/internal/shared/sqlite-database-integration';
if ( ! file_exists( $sqlite . '/wp-includes/sqlite/db.php' ) ) {
    // Playground's bundled SQLite is not available. Leave $wpdb alone so
    // WordPress can fall back to its default (MySQL attempt will fail loudly
    // rather than silently). This is the failure mode we want to surface.
    return;
}

if ( ! defined( 'DATABASE_TYPE' ) ) {
    define( 'DATABASE_TYPE', 'sqlite' );
}
if ( ! defined( 'DB_ENGINE' ) ) {
    define( 'DB_ENGINE', 'sqlite' );
}

// Delegate to Playground's drop-in. It sets up $wpdb = new WP_SQLite_DB(...).
require_once $sqlite . '/wp-includes/sqlite/db.php';
