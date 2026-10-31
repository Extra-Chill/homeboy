<?php
/**
 * Plugin Name: Bench Plugins-Loaded Host Fixture
 * Description: Component-under-test fixture that consumes the bench-plugins-
 *              loaded-dep validation dependency. Workloads under tests/bench/
 *              assert the dep's `plugins_loaded` callback ran AND the dep's
 *              ability is in the Abilities API registry — proving the bench
 *              runner loads dep entry files before wp-settings.php fires the
 *              canonical bootstrap hooks (homeboy-extensions#426).
 */
