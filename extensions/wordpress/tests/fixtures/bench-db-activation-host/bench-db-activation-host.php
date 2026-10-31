<?php
/**
 * Plugin Name: Bench DB Activation Host Fixture
 * Description: Component-under-test fixture that consumes the
 *              bench-db-activation-dep validation dependency. Workloads under
 *              tests/bench/ assert the dep's DB-touching activation hook ran
 *              cleanly — proving the bench runner fires plugin activation
 *              AFTER wp-phpunit's install path has created the test tables
 *              (homeboy-extensions#431).
 */
