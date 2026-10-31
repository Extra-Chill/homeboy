<?php
/**
 * Plugin Name: Playground Workloads WP-CLI Plugin Fixture
 *
 * Minimal under-test plugin used by
 * WP Codebox configured workload smoke coverage.
 *
 * The fixture itself does nothing at runtime. The smoke supplies a
 * configured workload that uses the wp-cli step type to install and
 * activate a real WordPress.org plugin, then asserts the next step in
 * the same workload sees that plugin loaded — proving that workload
 * wp-cli steps now have the bundled WP-CLI command surface (homeboy-
 * extensions#454).
 */
