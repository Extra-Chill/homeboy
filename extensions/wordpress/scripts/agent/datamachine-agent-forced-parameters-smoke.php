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

    fwrite( STDOUT, "Data Machine agent forced parameters smoke passed.\n" );
}
