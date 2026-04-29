<?php
/**
 * Fixture test that must be filtered out by HOMEBOY_CHANGED_TEST_FILES.
 *
 * @package Homeboy\WordPress\Tests\Fixtures
 */

class UnselectedScopeTest extends WP_UnitTestCase {

    public function test_unselected_file_would_fail_if_loaded() {
        $this->fail( 'Changed-test scope did not filter this file out.' );
    }
}
