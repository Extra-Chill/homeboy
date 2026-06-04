<?php
/**
 * Smoke test for Data Machine agent terminal-state handling and scenario output.
 *
 * Run with: php wordpress/scripts/agent/datamachine-agent-terminal-state-smoke.php
 */

namespace DataMachine\Core\Database\Jobs {
    class Jobs {
        public function get_job( int $job_id ): array {
            $GLOBALS['homeboy_terminal_state_reads'] = ( $GLOBALS['homeboy_terminal_state_reads'] ?? 0 ) + 1;
            return array(
                'job_id' => $job_id,
                'status' => ( $GLOBALS['homeboy_terminal_state_reads'] ?? 0 ) >= 3 ? 'completed' : 'pending',
            );
        }

        public function get_children( int $parent_job_id ): array {
            return array(
                array(
                    'job_id'        => 456,
                    'parent_job_id' => $parent_job_id,
                    'status'        => 'pending',
                ),
            );
        }
    }
}

namespace DataMachine\Core\Database\Agents {
    class Agents {}
}

namespace DataMachine\Core\Database\Chat {
    class ConversationStoreFactory {}
}

namespace DataMachine\Core\Database\Flows {
    class Flows {}
}

namespace DataMachine\Core\Database\Pipelines {
    class Pipelines {}
}

namespace DataMachine\Core {
    class PluginSettings {}
}

namespace {
    if ( ! defined( 'ABSPATH' ) ) {
        define( 'ABSPATH', __DIR__ . '/' );
    }

    if ( ! function_exists( 'wp_set_current_user' ) ) {
        function wp_set_current_user( int $user_id ): void {}
    }

    if ( ! function_exists( 'wp_get_ability' ) ) {
        function wp_get_ability( string $ability_name ): object {
            return new class {
                public function execute( array $args ): array {
                    $GLOBALS['homeboy_terminal_state_drain_calls'] = ( $GLOBALS['homeboy_terminal_state_drain_calls'] ?? 0 ) + 1;
                    return array(
                        'success' => true,
                        'job_id'  => (int) ( $args['job_id'] ?? 0 ),
                    );
                }
            };
        }
    }

    if ( ! function_exists( 'datamachine_get_engine_data' ) ) {
        function datamachine_get_engine_data( int $job_id ): array {
            return array();
        }
    }

    if ( ! function_exists( 'wp_json_encode' ) ) {
        function wp_json_encode( $value, int $flags = 0 ): string {
            return (string) json_encode( $value, $flags );
        }
    }

    putenv(
        'HOMEBOY_DATAMACHINE_AGENT_CONFIG=' . wp_json_encode(
            array(
                'dry_run'    => true,
                'agent_slug' => 'terminal-state-smoke-agent',
                'flow_slug'  => 'terminal-state-smoke-flow',
            )
        )
    );

    require __DIR__ . '/datamachine-agent-workload.php';

    $summary = homeboy_datamachine_agent_drain_job(
        123,
        array(
            'terminal_wait_budget_ms' => 250,
            'terminal_poll_sleep_ms'  => 1,
            'step_budget'             => 1,
            'time_budget_ms'          => 1000,
        ),
        new \DataMachine\Core\Database\Jobs\Jobs()
    );

    if ( empty( $summary['job_terminal'] ) || 'completed' !== $summary['final_job_status'] ) {
        fwrite( STDERR, "Expected drain to wait until the job reached completed status.\n" );
        exit( 1 );
    }

    if ( ( $GLOBALS['homeboy_terminal_state_drain_calls'] ?? 0 ) < 2 ) {
        fwrite( STDERR, "Expected drain to keep polling after an initial successful non-terminal drain pass.\n" );
        exit( 1 );
    }

    $result = homeboy_datamachine_agent_result(
        array( 'job_completed' => 1 ),
        array(
            'task_id'     => 'store-idea-agent',
            'engine_data' => array(
                'store_idea_agent' => array(
                    'issue_number' => 123,
                ),
            ),
        )
    );

    if ( 123 !== ( $result['scenarios'][0]['metadata']['engine_data']['store_idea_agent']['issue_number'] ?? 0 ) ) {
        fwrite( STDERR, "Expected workload result scenarios to expose engine_data.\n" );
        exit( 1 );
    }

    $child_error = homeboy_datamachine_agent_job_terminal_error( 456, 'pending', 'Data Machine child job' );
    if ( ! str_contains( $child_error, 'did not reach a terminal state' ) ) {
        fwrite( STDERR, "Expected non-terminal child job diagnostic to be precise.\n" );
        exit( 1 );
    }

    fwrite( STDOUT, "Data Machine agent terminal state smoke passed.\n" );
}
