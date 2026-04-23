<?php
/**
 * Plugin Name: Drop-in Coexistence Fixture
 * Description: Fixture plugin for the Playground backend's db.php drop-in coexistence smoke test. Not for production use.
 * Version: 1.0.0
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

// The fixture is deliberately inert at the plugin-entry layer. All the
// coexistence logic lives in the sibling db.php file. The test class in
// tests/ asserts that the drop-in ran and $wpdb still functions end-to-end.
