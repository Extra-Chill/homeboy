<?php

class InitLifecycleTest extends WP_UnitTestCase {
    public function test_deferred_init_callback_runs_in_init_context() {
        $this->assertSame(
            'init',
            get_option( 'homeboy_playground_init_lifecycle_context' ),
            'Deferred init callbacks must run with doing_action("init") true.'
        );
    }
}
