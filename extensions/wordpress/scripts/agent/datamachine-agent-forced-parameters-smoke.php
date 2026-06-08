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

    $GLOBALS['homeboy_forced_parameters_filters'] = array();
    $GLOBALS['homeboy_forced_parameters_options'] = array();

    if ( ! function_exists( 'add_filter' ) ) {
        function add_filter( string $tag, callable $callback, int $priority = 10, int $accepted_args = 1 ): void {
            $GLOBALS['homeboy_forced_parameters_filters'][ $tag ][ $priority ][] = $callback;
        }
    }

    if ( ! function_exists( 'apply_filters' ) ) {
        function apply_filters( string $tag, $value ) {
            if ( empty( $GLOBALS['homeboy_forced_parameters_filters'][ $tag ] ) ) {
                return $value;
            }
            ksort( $GLOBALS['homeboy_forced_parameters_filters'][ $tag ] );
            foreach ( $GLOBALS['homeboy_forced_parameters_filters'][ $tag ] as $callbacks ) {
                foreach ( $callbacks as $callback ) {
                    $value = $callback( $value );
                }
            }
            return $value;
        }
    }

    if ( ! function_exists( 'wp_get_ability' ) ) {
        function wp_get_ability( string $ability_name ): object {
            $GLOBALS['homeboy_forced_parameters_ability_names'][] = $ability_name;
            return new class( $ability_name ) {
                private string $ability_name;

                public function __construct( string $ability_name ) {
                    $this->ability_name = $ability_name;
                }

                public function execute( array $args ): array {
                    $GLOBALS['homeboy_forced_parameters_args'] = $args;
                    if ( 'datamachine-code/workspace-worktree-add' === $this->ability_name ) {
                        return array(
                            'success' => true,
                            'handle'  => $args['repo'] . '@' . str_replace( '/', '-', (string) $args['branch'] ),
                            'branch'  => (string) $args['branch'],
                            'path'    => '/tmp/' . $args['repo'],
                            'args'    => $args,
                        );
                    }
                    return array(
                        'success' => true,
                        'args'    => $args,
                    );
                }
            };
        }
    }

    if ( ! function_exists( 'get_option' ) ) {
        function get_option( string $option, $default = false ) {
            return array_key_exists( $option, $GLOBALS['homeboy_forced_parameters_options'] ) ? $GLOBALS['homeboy_forced_parameters_options'][ $option ] : $default;
        }
    }

    if ( ! function_exists( 'update_option' ) ) {
        function update_option( string $option, $value, $autoload = null ): bool {
            $GLOBALS['homeboy_forced_parameters_options'][ $option ] = $value;
            return true;
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
                'dry_run'     => true,
                'bundle_path' => __DIR__,
                'agent_slug'  => 'forced-parameters-smoke-agent',
                'flow_slug'   => 'forced-parameters-smoke-flow',
                'prompt'      => 'Dry-run the fingerprint smoke test.',
                'tool_audit_events' => array(
                    array(
                        'schema_version'      => 1,
                        'type'                => 'tool_call',
                        'turn_count'          => 1,
                        'tool_name'           => 'client/search_docs',
                        'tool_source'         => 'client',
                        'parameters_sha256'   => 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                        'parameters_redacted' => true,
                        'success'             => true,
                        'result_status'       => 'success',
                        'result_sha256'       => 'sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                    ),
                ),
            )
        )
    );

    $dry_run_result = require __DIR__ . '/datamachine-agent-workload.php';

    $fingerprints = is_array( $dry_run_result ) ? ( $dry_run_result['metadata']['fingerprints'] ?? array() ) : array();
    if ( empty( $fingerprints['prompt']['sha256'] ) ) {
        fwrite( STDERR, "Expected dry-run result to include a prompt fingerprint.\n" );
        exit( 1 );
    }

    if ( empty( $fingerprints['bundle']['sha256'] ) || empty( $fingerprints['bundle']['file_count'] ) ) {
        fwrite( STDERR, "Expected dry-run result to include a bundle fingerprint.\n" );
        exit( 1 );
    }

    if ( empty( $fingerprints['tool_policy']['sha256'] ) ) {
        fwrite( STDERR, "Expected dry-run result to include a tool-policy fingerprint.\n" );
        exit( 1 );
    }

    $eval_artifact = is_array( $dry_run_result ) ? ( $dry_run_result['metadata']['eval_artifact'] ?? array() ) : array();
    if ( 'homeboy.agent_eval_result' !== ( $eval_artifact['schema_name'] ?? '' ) ) {
        fwrite( STDERR, "Expected dry-run result to include canonical eval artifact schema.\n" );
        exit( 1 );
    }

    if ( 'forced-parameters-smoke-agent' !== ( $eval_artifact['agent']['slug'] ?? '' ) ) {
        fwrite( STDERR, "Expected eval artifact to identify the agent slug.\n" );
        exit( 1 );
    }

    if ( 1 !== ( $eval_artifact['replay']['tool_audit_event_count'] ?? 0 ) ) {
        fwrite( STDERR, "Expected eval artifact to include Agents API tool audit events.\n" );
        exit( 1 );
    }

    $integration_seams = $eval_artifact['attestation']['integration_seams'] ?? array();
    if ( ! in_array( 'datamachine_provenance', $integration_seams, true ) || ! in_array( 'datamachine_code_policy_attestation', $integration_seams, true ) ) {
        fwrite( STDERR, "Expected eval artifact to expose missing provenance and policy attestation seams.\n" );
        exit( 1 );
    }

    $GLOBALS['homeboy_forced_parameters_filters'] = array();
    homeboy_datamachine_agent_register_tool_recorders( array() );
    $default_tools = apply_filters(
        'datamachine_resolved_tools',
        array(
            'create_github_pull_request'  => array( 'class' => 'Default_PR_Tool' ),
            'create_or_update_github_file' => array( 'class' => 'Default_File_Tool' ),
        )
    );
    if ( isset( $default_tools['create_github_pull_request'] ) ) {
        fwrite( STDERR, "Expected default tool recorders to remove PR publication from the task-facing tool surface.\n" );
        exit( 1 );
    }
    if ( empty( $default_tools['create_or_update_github_file']['homeboy_record'] ) ) {
        fwrite( STDERR, "Expected default file publication recorder to remain available.\n" );
        exit( 1 );
    }

    $GLOBALS['homeboy_forced_parameters_filters'] = array();
    homeboy_datamachine_agent_register_tool_recorders(
        array(
            'tool_recorders' => array(
                array( 'tool' => 'create_github_pull_request' ),
            ),
        )
    );
    $explicit_tools = apply_filters(
        'datamachine_resolved_tools',
        array(
            'create_github_pull_request' => array( 'class' => 'Explicit_PR_Tool' ),
        )
    );
    if ( empty( $explicit_tools['create_github_pull_request']['homeboy_original_tool'] ) ) {
        fwrite( STDERR, "Expected explicit PR tool recorder to preserve the task-facing PR tool.\n" );
        exit( 1 );
    }

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

    list( $alias_config, $alias_prompt ) = homeboy_datamachine_agent_apply_runner_workspace(
        array(
            'runner_workspace' => array(
                'enabled'     => true,
                'agent_alias' => 'current-project',
                'agent_root'  => '.agent-workspace/current-project',
            ),
        ),
        'Make the requested code changes.',
        array(
            'success' => true,
            'handle'  => 'demo@agent-run-openai-gpt-5-5',
            'branch'  => 'agent/run-openai-gpt-5-5',
        )
    );

    if ( str_contains( $alias_prompt, 'demo@agent-run-openai-gpt-5-5' ) || str_contains( $alias_prompt, 'agent/run-openai-gpt-5-5' ) ) {
        fwrite( STDERR, "Expected alias runner prompt to hide the real workspace handle and branch.\n" );
        exit( 1 );
    }

    if ( ! str_contains( $alias_prompt, 'current-project' ) ) {
        fwrite( STDERR, "Expected alias runner prompt to mention the opaque project alias.\n" );
        exit( 1 );
    }

    $alias_workspace_recorder = null;
    foreach ( $alias_config['tool_recorders'] ?? array() as $runner_recorder ) {
        if ( is_array( $runner_recorder ) && 'workspace_git_status' === ( $runner_recorder['tool'] ?? '' ) ) {
            $alias_workspace_recorder = $runner_recorder;
            break;
        }
    }

    if ( 'demo@agent-run-openai-gpt-5-5' !== ( $alias_workspace_recorder['forced_parameters']['name'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to dispatch git tools to the real workspace handle.\n" );
        exit( 1 );
    }

    if ( 'current-project' !== ( $alias_workspace_recorder['workspace_alias']['alias'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to retain the opaque alias for response sanitization.\n" );
        exit( 1 );
    }

    $aliases = apply_filters( 'datamachine_code_workspace_aliases', array() );
    if ( 'demo@agent-run-openai-gpt-5-5' !== ( $aliases['current-project']['target'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to register the opaque alias mapping.\n" );
        exit( 1 );
    }

    if ( '.agent-workspace/current-project' !== ( $aliases['current-project']['root'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to register the scoped alias root.\n" );
        exit( 1 );
    }

    $persisted_aliases = get_option( 'datamachine_code_workspace_aliases', array() );
    if ( 'demo@agent-run-openai-gpt-5-5' !== ( $persisted_aliases['current-project']['target'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to persist the opaque alias mapping for tool requests.\n" );
        exit( 1 );
    }

    if ( '.agent-workspace/current-project' !== ( $persisted_aliases['current-project']['root'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to persist the scoped alias root for tool requests.\n" );
        exit( 1 );
    }

    $scoped_parameters = homeboy_datamachine_agent_apply_runner_workspace_alias_parameters(
        array(
            'repo' => 'demo@agent-run-openai-gpt-5-5',
            'path' => 'plugins/sample.php',
        ),
        $alias_workspace_recorder['workspace_alias']
    );

    if ( '.agent-workspace/current-project/plugins/sample.php' !== ( $scoped_parameters['path'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to scope workspace file paths before dispatch.\n" );
        exit( 1 );
    }

    $GLOBALS['homeboy_forced_parameters_ability_names'] = array();
    $provisioned_workspace = homeboy_datamachine_agent_provision_workspace(
        array(
            'runner_workspace' => array(
                'enabled'       => true,
                'repo'          => 'html-to-blocks-converter',
                'branch'        => 'agent/iterator-repair',
                'allow_stale'   => true,
                'rebase_base'   => true,
                'clone_url'     => 'https://github.com/chubes4/html-to-blocks-converter.git',
            ),
        )
    );

    if ( empty( $provisioned_workspace['success'] ) || 'html-to-blocks-converter@agent-iterator-repair' !== ( $provisioned_workspace['handle'] ?? null ) ) {
        fwrite( STDERR, "Expected runner workspace provisioning to create a DMC worktree.\n" );
        exit( 1 );
    }

    foreach ( array( 'datamachine-code/workspace-show', 'datamachine-code/workspace-clone', 'datamachine-code/workspace-worktree-add' ) as $expected_ability ) {
        if ( ! in_array( $expected_ability, $GLOBALS['homeboy_forced_parameters_ability_names'], true ) ) {
            fwrite( STDERR, "Expected runner workspace provisioning to resolve {$expected_ability}.\n" );
            exit( 1 );
        }
    }

    foreach ( $GLOBALS['homeboy_forced_parameters_ability_names'] as $ability_name ) {
        if ( str_starts_with( $ability_name, 'datamachine/workspace-' ) ) {
            fwrite( STDERR, "Runner workspace provisioning used stale workspace ability namespace.\n" );
            exit( 1 );
        }
    }

    $sanitized_response = homeboy_datamachine_agent_sanitize_runner_workspace_alias_result(
        array(
            'success' => true,
            'data'    => array(
                'repo' => 'demo@agent-run-openai-gpt-5-5',
                'path' => '.agent-workspace/current-project/plugins/sample.php',
            ),
        ),
        $alias_workspace_recorder['workspace_alias']
    );

    if ( 'current-project' !== ( $sanitized_response['data']['repo'] ?? null ) || 'plugins/sample.php' !== ( $sanitized_response['data']['path'] ?? null ) ) {
        fwrite( STDERR, "Expected alias runner workspace mode to sanitize workspace tool responses.\n" );
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
