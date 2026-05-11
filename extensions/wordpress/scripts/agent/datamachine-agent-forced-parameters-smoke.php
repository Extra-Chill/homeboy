<?php
/**
 * Smoke test for forced Data Machine agent tool parameters.
 *
 * Run with: php wordpress/scripts/agent/datamachine-agent-forced-parameters-smoke.php
 */

namespace DataMachine\Core\Database\Agents {
    class Agents {}
}

namespace DataMachine\Core\Database\Chat {
    class ConversationStoreFactory {}
}

namespace DataMachine\Core\Database\Flows {
    class Flows {}
}

namespace DataMachine\Core\Database\Jobs {
    class Jobs {}
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
                    $GLOBALS['homeboy_forced_parameters_args'] = $args;
                    return array(
                        'success' => true,
                        'args'    => $args,
                    );
                }
            };
        }
    }

    if ( ! function_exists( 'is_wp_error' ) ) {
        function is_wp_error( $value ): bool {
            return false;
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
                'agent_slug' => 'forced-parameters-smoke-agent',
                'flow_slug'  => 'forced-parameters-smoke-flow',
            )
        )
    );

    require __DIR__ . '/datamachine-agent-workload.php';

    $recorder = new Homeboy_Datamachine_Agent_Tool_Recorder();
    $result   = $recorder->handle_tool_call(
        array(
            'repo'               => 'owner/repo',
            'file_path'          => 'docs/generated.md',
            'content'            => '# Generated',
            'commit_message'     => 'docs: generated',
            'allowed_file_paths' => array( 'src/**' ),
        ),
        array(
            'ability'                    => 'datamachine/create-or-update-github-file',
            'tool_name'                  => 'create_or_update_github_file',
            'homeboy_forced_parameters' => array(
                'allowed_file_paths' => array( 'README.md', 'docs/**' ),
            ),
        )
    );

    $args = $GLOBALS['homeboy_forced_parameters_args'] ?? array();
    if ( empty( $result['success'] ) ) {
        fwrite( STDERR, "Expected forced-parameter tool call to succeed.\n" );
        exit( 1 );
    }

    if ( array( 'README.md', 'docs/**' ) !== ( $args['allowed_file_paths'] ?? null ) ) {
        fwrite( STDERR, "Expected forced allowed_file_paths to override model parameters.\n" );
        exit( 1 );
    }

    list( $runner_config, $runner_prompt ) = homeboy_datamachine_agent_apply_runner_workspace(
        array(),
        'Run the agent.',
        array(
            'success' => true,
            'handle'  => 'demo@agent-run',
            'branch'  => 'agent/run',
        )
    );

    if ( ! str_contains( $runner_prompt, 'Workspace handle: demo@agent-run' ) ) {
        fwrite( STDERR, "Expected runner workspace prompt prefix.\n" );
        exit( 1 );
    }

    $workspace_recorder = null;
    foreach ( $runner_config['tool_recorders'] ?? array() as $runner_recorder ) {
        if ( is_array( $runner_recorder ) && 'workspace_edit' === ( $runner_recorder['tool'] ?? '' ) ) {
            $workspace_recorder = $runner_recorder;
            break;
        }
    }

    if ( 'demo@agent-run' !== ( $workspace_recorder['forced_parameters']['repo'] ?? null ) ) {
        fwrite( STDERR, "Expected runner workspace tools to force the worktree handle.\n" );
        exit( 1 );
    }

    if ( 'github_tool_results' !== ( $workspace_recorder['record']['tool_results_key'] ?? null ) ) {
        fwrite( STDERR, "Expected runner workspace tools to record tool results.\n" );
        exit( 1 );
    }

    list( $hidden_config, $hidden_prompt ) = homeboy_datamachine_agent_apply_runner_workspace(
        array(
            'runner_workspace' => array(
                'enabled'         => true,
                'expose_to_agent' => false,
            ),
        ),
        'Run the agent naturally.',
        array(
            'success' => true,
            'handle'  => 'demo@hidden-run',
            'branch'  => 'agent/hidden-run',
        )
    );

    if ( 'Run the agent naturally.' !== $hidden_prompt ) {
        fwrite( STDERR, "Expected hidden runner workspace mode to preserve the natural prompt.\n" );
        exit( 1 );
    }

    $hidden_workspace_recorder = null;
    foreach ( $hidden_config['tool_recorders'] ?? array() as $runner_recorder ) {
        if ( is_array( $runner_recorder ) && 'workspace_write' === ( $runner_recorder['tool'] ?? '' ) ) {
            $hidden_workspace_recorder = $runner_recorder;
            break;
        }
    }

    if ( 'demo@hidden-run' !== ( $hidden_workspace_recorder['forced_parameters']['repo'] ?? null ) ) {
        fwrite( STDERR, "Expected hidden runner workspace mode to keep workspace tool scoping.\n" );
        exit( 1 );
    }

    $implicit_fallback = homeboy_datamachine_agent_open_fallback_pr(
        array(),
        array(
            'target_repo'             => 'owner/demo',
            'agent_slug'              => 'demo-agent',
            'model'                   => 'gpt-5.5',
            'runner_workspace_result' => array(
                'success' => true,
                'branch'  => 'agent/run',
            ),
        )
    );

    if ( ! empty( $implicit_fallback['opened'] ) ) {
        fwrite( STDERR, "Did not expect runner workspace fallback PR to open without explicit fallback config.\n" );
        exit( 1 );
    }

    $fallback = homeboy_datamachine_agent_open_fallback_pr(
        array(),
        array(
            'target_repo'             => 'owner/demo',
            'agent_slug'              => 'demo-agent',
            'model'                   => 'gpt-5.5',
            'fallback_pull_request'   => array(
                'repo'  => 'owner/demo',
                'head'  => 'agent/run',
                'title' => 'Fallback PR',
            ),
            'runner_workspace_result' => array(
                'success' => true,
                'branch'  => 'agent/run',
            ),
        )
    );

    if ( empty( $fallback['opened'] ) ) {
        fwrite( STDERR, "Expected explicit fallback PR to open.\n" );
        exit( 1 );
    }

    $fallback_args = $GLOBALS['homeboy_forced_parameters_args'] ?? array();
    if ( 'agent/run' !== ( $fallback_args['head'] ?? null ) || 'owner/demo' !== ( $fallback_args['repo'] ?? null ) ) {
        fwrite( STDERR, "Expected fallback PR to target the runner workspace branch.\n" );
        exit( 1 );
    }

    fwrite( STDOUT, "Data Machine agent forced parameters smoke passed.\n" );
}
