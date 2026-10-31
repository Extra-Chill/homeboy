<?php
/**
 * Plugin Name: Test Plugins-Loaded Host Fixture
 * Description: PHPUnit-side counterpart to bench-plugins-loaded-host. Used by
 *              the test runner smoke for homeboy-extensions#426 to lock in
 *              the test runner's existing pre-install dep load behavior as a
 *              regression guard so any future refactor of the test runner's
 *              boot order keeps validation-dependency `plugins_loaded`
 *              callbacks live before wp-settings.php fires lifecycle hooks.
 */
