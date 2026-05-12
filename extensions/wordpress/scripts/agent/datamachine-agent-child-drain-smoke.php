<?php
/**
 * Smoke test for Data Machine agent child-job draining and result aggregation.
 *
 * Run with: php wordpress/scripts/agent/datamachine-agent-child-drain-smoke.php
 */

namespace DataMachine\Core\Database\Jobs {
    class Jobs {
        public function get_job( int $job_id ): array {
            return array(
                'job_id' => $job_id,
                'status' => 'completed',
            );
        }

        public function get_children( int $parent_job_id ): array {
            return array(
                array(
                    'job_id'        => 201,
                    'parent_job_id' => $parent_job_id,
                    'status'        => 'completed',
                    'engine_data'   => array(
                        'github_tool_results' => array(
                            array(
                                'success' => true,
                                'url'     => 'https://github.com/example/repo/pull/12',
                            ),
                        ),
                    ),
                ),
                array(
                    'job_id'        => 202,
                    'parent_job_id' => $parent_job_id,
                    'status'        => 'completed',
                    'engine_data'   => array(
                        'github_tool_results' => array(
                            array(
                                'success'   => true,
                                'tool_name' => 'comment_github_pull_request',
                                'url'       => 'https://github.com/example/site/pull/99#issuecomment-1',
                            ),
                        ),
                    ),
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
                    $GLOBALS['homeboy_child_drain_calls'][] = (int) ( $args['job_id'] ?? 0 );
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
                'agent_slug' => 'child-drain-smoke-agent',
                'flow_slug'  => 'child-drain-smoke-flow',
            )
        )
    );

	require __DIR__ . '/datamachine-agent-workload.php';

	$workload_source = file_get_contents( __DIR__ . '/datamachine-agent-workload.php' ) ?: '';
	$import_position = strpos( $workload_source, "wp_get_ability( 'datamachine/import-agent' )" );
	$execute_position = strpos( $workload_source, "wp_get_ability( 'datamachine/execute-workflow' )" );
	if ( false === $import_position || false === $execute_position || $import_position > $execute_position ) {
		fwrite( STDERR, "Expected execute-workflow mode to import the agent bundle before running the raw workflow.\n" );
		exit( 1 );
	}
	if ( ! str_contains( $workload_source, "array( 'datamachine/import-agent', 'datamachine/execute-workflow', 'datamachine/drain-job' )" ) ) {
		fwrite( STDERR, "Expected execute-workflow mode to require import-agent, execute-workflow, and drain-job.\n" );
		exit( 1 );
	}
	$workflow_source = file_get_contents( dirname( __DIR__, 3 ) . '/.github/workflows/datamachine-agent-ci.yml' ) ?: '';
	if ( ! str_contains( $workflow_source, '["datamachine/import-agent", "datamachine/execute-workflow", "datamachine/drain-job"]' ) ) {
		fwrite( STDERR, "Expected reusable workflow config to require import-agent for execute-workflow runs.\n" );
		exit( 1 );
	}

	$jobs = new \DataMachine\Core\Database\Jobs\Jobs();
    $summary = homeboy_datamachine_agent_drain_child_jobs( 101, array(), $jobs );
    $calls = $GLOBALS['homeboy_child_drain_calls'] ?? array();

    if ( array( 201, 202 ) !== $calls ) {
        fwrite( STDERR, "Expected child jobs 201 and 202 to be drained.\n" );
        exit( 1 );
    }

    if ( 2 !== count( $summary['children'] ?? array() ) ) {
        fwrite( STDERR, "Expected two child jobs in the summary.\n" );
        exit( 1 );
    }

    $engine_data = homeboy_datamachine_agent_merge_child_engine_data( array(), $summary['children'], array() );
    if ( ! homeboy_datamachine_agent_pr_opened( $engine_data, array() ) ) {
        fwrite( STDERR, "Expected child pull request result to satisfy PR detection.\n" );
        exit( 1 );
    }

    fwrite( STDOUT, "Data Machine agent child drain smoke passed.\n" );
}
