<?php
/**
 * Plugin Name: Test DB Activation Host Fixture
 * Description: Component-under-test fixture that consumes the
 *              bench-db-activation-dep validation dependency. PHPUnit tests
 *              under tests/ assert the dep's DB-touching activation hook ran
 *              cleanly — proving the test runner fires plugin activation
 *              AFTER wp-phpunit's install path has created the test tables
 *              (homeboy-extensions#431).
 */
