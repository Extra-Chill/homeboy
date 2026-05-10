<?php
/**
 * Smoke test for retry-aware Data Machine agent drain loop.
 *
 * Run with: php wordpress/scripts/agent/datamachine-agent-retry-drain-smoke.php
 */

namespace DataMachine\Core\Database\Jobs {
    class Jobs {
        public function get_job( int $job_id ): array {
            $GLOBALS['homeboy_retry_drain_job_reads'] = ( $GLOBALS['homeboy_retry_drain_job_reads'] ?? 0 ) + 1;
            return array(
                'job_id' => $job_id,
                'status' => ( $GLOBALS['homeboy_retry_drain_calls'] ?? 0 ) >= 2 ? 'completed' : 'pending',
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
                    $GLOBALS['homeboy_retry_drain_calls'] = ( $GLOBALS['homeboy_retry_drain_calls'] ?? 0 ) + 1;
                    if ( 1 === $GLOBALS['homeboy_retry_drain_calls'] ) {
                        return array(
                            'success'           => false,
                            'job_id'            => (int) ( $args['job_id'] ?? 0 ),
                            'remaining_actions' => 0,
                        );
                    }

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
            return array(
                'retry' => array(
                    'last_retryable' => true,
                    'next_retry_at'  => gmdate( 'c', time() - 1 ),
                ),
            );
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
                'agent_slug' => 'retry-smoke-agent',
                'flow_slug'  => 'retry-smoke-flow',
            )
        )
    );

    require __DIR__ . '/datamachine-agent-workload.php';

    $summary = homeboy_datamachine_agent_drain_job(
        123,
        array(
            'retry_wait_budget_ms' => 1,
            'retry_max_sleep_ms'   => 1,
            'step_budget'          => 1,
            'time_budget_ms'       => 1000,
        ),
        new \DataMachine\Core\Database\Jobs\Jobs()
    );

    if ( 2 !== ( $GLOBALS['homeboy_retry_drain_calls'] ?? 0 ) ) {
        fwrite( STDERR, "Expected two drain attempts.\n" );
        exit( 1 );
    }

    if ( empty( $summary['drain_result']['success'] ) ) {
        fwrite( STDERR, "Expected retry drain to finish successfully.\n" );
        exit( 1 );
    }

    if ( 2 !== count( $summary['drain_history'] ?? array() ) ) {
        fwrite( STDERR, "Expected two drain history entries.\n" );
        exit( 1 );
    }

    fwrite( STDOUT, "Retry-aware Data Machine agent drain smoke passed.\n" );
}
