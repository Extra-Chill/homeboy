<?php
/**
 * Smoke test for Data Machine agent write-without-PR classification.
 *
 * Run with: php wordpress/scripts/agent/datamachine-agent-write-without-pr-smoke.php
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
    class JobArtifacts {
        public function get( int $job_id, array $additional_tool_summaries = array() ): array {
            return array(
                'success'   => true,
                'artifacts' => array(
                    'transcript'             => array( 'session_id' => 'session-123' ),
                    'agent_memory_artifacts' => array(),
                    'daily_memory_artifacts' => array(),
                ),
            );
        }
    }

    class PluginSettings {
        public static function clearCache(): void {}
    }
}

namespace {
    if ( ! defined( 'ABSPATH' ) ) {
        define( 'ABSPATH', __DIR__ . '/' );
    }

    if ( ! function_exists( 'wp_json_encode' ) ) {
        function wp_json_encode( $value, int $flags = 0 ): string {
            return (string) json_encode( $value, $flags );
        }
    }

    if ( ! function_exists( 'is_wp_error' ) ) {
        function is_wp_error( $value ): bool {
            return false;
        }
    }

    if ( ! function_exists( 'get_option' ) ) {
        function get_option( string $name, $default = false ) {
            return $GLOBALS['homeboy_datamachine_agent_fake_options'][ $name ] ?? $default;
        }
    }

    if ( ! function_exists( 'update_option' ) ) {
        function update_option( string $name, $value, bool $autoload = true ): bool {
            $GLOBALS['homeboy_datamachine_agent_fake_options'][ $name ] = $value;
            return true;
        }
    }

    class Homeboy_Datamachine_Agent_Fake_Ability {
        private $callback;

        public function __construct( callable $callback ) {
            $this->callback = $callback;
        }

        public function execute( array $input ): array {
            return ( $this->callback )( $input );
        }
    }

    if ( ! function_exists( 'wp_get_ability' ) ) {
        function wp_get_ability( string $name ) {
            return $GLOBALS['homeboy_datamachine_agent_fake_abilities'][ $name ] ?? null;
        }
    }

    putenv(
        'HOMEBOY_DATAMACHINE_AGENT_CONFIG=' . wp_json_encode(
            array(
                'dry_run'    => true,
                'agent_slug' => 'write-without-pr-smoke-agent',
                'flow_slug'  => 'write-without-pr-smoke-flow',
            )
        )
    );

    require __DIR__ . '/datamachine-agent-workload.php';

    $flow_prompt_queue = homeboy_datamachine_agent_run_prompt_queue(
        array(
            'prompt_queue' => array(
                array( 'prompt' => 'Use the bundled flow prompt.' ),
            ),
        ),
        ''
    );

    if ( 'Use the bundled flow prompt.' !== ( $flow_prompt_queue[0]['prompt'] ?? '' ) ) {
        fwrite( STDERR, "Expected an empty run prompt to preserve the bundled flow prompt.\n" );
        exit( 1 );
    }

    $combined_prompt_queue = homeboy_datamachine_agent_run_prompt_queue(
        array(
            'prompt_queue' => array(
                array( 'prompt' => 'Use the bundled flow prompt.' ),
            ),
        ),
        'Apply this run context.'
    );

    if ( "Use the bundled flow prompt.\n\nRun context:\nApply this run context." !== ( $combined_prompt_queue[0]['prompt'] ?? '' ) ) {
        fwrite( STDERR, "Expected a run prompt to be appended to the bundled flow prompt.\n" );
        exit( 1 );
    }

    $config = array( 'tool_results_key' => 'github_tool_results' );

    $write_without_pr = array(
        'github_tool_results' => array(
            array(
                'tool_name' => 'create_or_update_github_file',
                'success'   => true,
                'url'       => 'https://github.com/owner/repo/commit/abc123',
            ),
        ),
    );

    if ( ! homeboy_datamachine_agent_file_written( $write_without_pr, $config ) ) {
        fwrite( STDERR, "Expected successful file-write tool result to count as file_written.\n" );
        exit( 1 );
    }

    if ( homeboy_datamachine_agent_pr_opened( $write_without_pr, $config ) ) {
        fwrite( STDERR, "Did not expect commit-only result to count as pr_opened.\n" );
        exit( 1 );
    }

    $pr_opened = array(
        'github_tool_results' => array(
            array(
                'tool_name' => 'create_github_pull_request',
                'success'   => true,
                'head'      => 'agent/change-branch',
                'url'       => 'https://github.com/owner/repo/pull/123',
            ),
        ),
    );

    if ( ! homeboy_datamachine_agent_pr_opened( $pr_opened, $config ) ) {
        fwrite( STDERR, "Expected pull-request URL to count as pr_opened.\n" );
        exit( 1 );
    }

    if ( 'agent/change-branch' !== homeboy_datamachine_agent_pr_head_branch( $pr_opened, $config ) ) {
        fwrite( STDERR, "Expected pull-request tool result to expose the PR head branch.\n" );
        exit( 1 );
    }

    $nested_config = array(
        'engine_key'       => 'docs_agent',
        'tool_results_key' => 'github_tool_results',
    );

    $nested_results = array(
        'docs_agent' => array(
            'github_tool_results' => array(
                array(
                    'tool_name' => 'create_or_update_github_file',
                    'success'   => true,
                    'url'       => 'https://github.com/owner/repo/commit/def456',
                ),
                array(
                    'tool_name' => 'create_github_pull_request',
                    'success'   => true,
                    'url'       => 'https://github.com/owner/repo/pull/456',
                ),
            ),
        ),
    );

    if ( ! homeboy_datamachine_agent_file_written( $nested_results, $nested_config ) || ! homeboy_datamachine_agent_pr_opened( $nested_results, $nested_config ) ) {
        fwrite( STDERR, "Expected nested engine-key tool results to drive write and PR classification.\n" );
        exit( 1 );
    }

    $recorded_results = new ReflectionProperty( Homeboy_Datamachine_Agent_Tool_Recorder::class, 'tool_results' );
    $recorded_results->setValue( null, array(
        array(
            'tool_name' => 'create_github_pull_request',
            'success'   => true,
            'repo'      => 'owner/repo',
            'head'      => 'agent/recorded-branch',
            'url'       => 'https://github.com/owner/repo/pull/789',
        ),
    ) );
    $merged_recorded_results = homeboy_datamachine_agent_merge_recorded_tool_results( array(), $config );
    if ( ! homeboy_datamachine_agent_pr_opened( $merged_recorded_results, $config ) ) {
        fwrite( STDERR, "Expected current-run recorded tool results to drive PR classification.\n" );
        exit( 1 );
    }

    if ( 'agent/recorded-branch' !== homeboy_datamachine_agent_pr_head_branch( $merged_recorded_results, $config ) ) {
        fwrite( STDERR, "Expected current-run recorded tool results to preserve the PR head branch.\n" );
        exit( 1 );
    }

    $GLOBALS['homeboy_datamachine_agent_fake_abilities'] = array(
        'datamachine/list-github-pulls' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn() => array(
                'success' => true,
                'pulls'   => array(
                    array(
                        'number'   => 321,
                        'html_url' => 'https://github.com/owner/repo/pull/321',
                        'head'     => 'agent/existing-pr-branch',
                        'base'     => 'main',
                    ),
                ),
            )
        ),
        'datamachine/create-github-pull-request' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function (): array {
                fwrite( STDERR, "Did not expect fallback PR creation when an existing PR matches the head branch.\n" );
                exit( 1 );
            }
        ),
    );
    $existing_fallback = homeboy_datamachine_agent_open_fallback_pr(
        array(),
        array(
            'tool_results_key'      => 'github_tool_results',
            'fallback_pull_request' => array(
                'repo'  => 'owner/repo',
                'title' => 'Fallback PR',
                'head'  => 'agent/existing-pr-branch',
                'base'  => 'main',
            ),
        )
    );
    if ( empty( $existing_fallback['opened'] ) || empty( $existing_fallback['reused'] ) ) {
        fwrite( STDERR, "Expected fallback PR handling to reuse an existing open PR.\n" );
        exit( 1 );
    }
    if ( ! homeboy_datamachine_agent_pr_opened( $existing_fallback['engine_data'] ?? array(), $config ) ) {
        fwrite( STDERR, "Expected reused fallback PR to be recorded as pr_opened.\n" );
        exit( 1 );
    }

    $fallback_pr_input = array();
    $GLOBALS['homeboy_datamachine_agent_fake_abilities'] = array(
        'datamachine/list-github-pulls' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn() => array(
                'success' => true,
                'pulls'   => array(),
            )
        ),
        'datamachine/create-github-pull-request' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function ( array $input ) use ( &$fallback_pr_input ): array {
                $fallback_pr_input = $input;
                return array(
                    'success'  => true,
                    'html_url' => 'https://github.com/owner/repo/pull/654',
                );
            }
        ),
    );
    $created_fallback = homeboy_datamachine_agent_open_fallback_pr(
        array(),
        array(
            'tool_results_key'      => 'github_tool_results',
            'fallback_pull_request' => array(
                'repo'  => 'owner/repo',
                'title' => 'Fallback PR',
                'head'  => 'agent/new-pr-branch',
            ),
        )
    );
    if ( empty( $created_fallback['opened'] ) ) {
        fwrite( STDERR, "Expected fallback PR handling to create a PR when no existing PR matches.\n" );
        exit( 1 );
    }
    if ( array_key_exists( 'base', $fallback_pr_input ) ) {
        fwrite( STDERR, "Expected fallback PR creation to omit an empty base parameter.\n" );
        exit( 1 );
    }

    $runner_capture_calls = array();
    $fallback_pr_input    = array();
    $GLOBALS['homeboy_datamachine_agent_fake_abilities'] = array(
        'datamachine/workspace-git-status' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function ( array $input ) use ( &$runner_capture_calls ): array {
                $runner_capture_calls[] = array( 'ability' => 'status', 'input' => $input );
                return array(
                    'success' => true,
                    'dirty'   => 1,
                    'files'   => array( 'docs/generated.md' ),
                );
            }
        ),
        'datamachine/workspace-git-diff' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn( array $input ) => array(
                'success' => true,
                'name'    => $input['name'] ?? '',
                'diff'    => "diff --git a/docs/generated.md b/docs/generated.md\n",
            )
        ),
        'datamachine/workspace-git-add' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function ( array $input ) use ( &$runner_capture_calls ): array {
                $runner_capture_calls[] = array( 'ability' => 'add', 'input' => $input );
                return array( 'success' => true, 'paths' => $input['paths'] ?? array() );
            }
        ),
        'datamachine/workspace-git-commit' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn( array $input ) => array( 'success' => true, 'commit' => 'abc123', 'message' => $input['message'] ?? '' )
        ),
        'datamachine/workspace-git-push' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn( array $input ) => array( 'success' => true, 'branch' => $input['branch'] ?? '', 'html_url' => 'https://github.com/owner/repo/tree/agent/hidden-run' )
        ),
        'datamachine/list-github-pulls' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn() => array( 'success' => true, 'pulls' => array() )
        ),
        'datamachine/create-github-pull-request' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function ( array $input ) use ( &$fallback_pr_input ): array {
                $fallback_pr_input = $input;
                return array(
                    'success'  => true,
                    'html_url' => 'https://github.com/owner/repo/pull/987',
                );
            }
        ),
    );
    putenv( 'GITHUB_REPOSITORY=owner/repo' );
    putenv( 'GITHUB_RUN_ID=123456' );
    $runner_config = array(
        'target_repo'              => 'owner/repo',
        'task_id'                  => 'runner-task',
        'task_label'               => 'Runner task',
        'provider'                 => 'openai',
        'model'                    => 'gpt-smoke',
        'agent_slug'               => 'runner-agent',
        'tool_results_key'         => 'github_tool_results',
        'artifact_export'          => array(
            'pr_title_template'  => '[{agent_slug}] {task_id} - {workspace_branch} - {result_label}',
            'pr_body_template'   => "## Runner Workspace\nBranch: {workspace_branch}\nHandle: {workspace_handle}\nStatus: {engine_status}\nCustom: {custom_label}\n\n## Checks\n{checks_table}\n\n## Links\n{links_table}\n\n## Paths\n{paths}\n",
            'pr_template_values' => array( 'custom_label' => 'custom fallback value' ),
            'pr_template_paths'  => array( 'engine_status' => 'engine_data.status' ),
        ),
        'runner_workspace'         => array(
            'enabled'         => true,
            'expose_to_agent' => false,
        ),
        'runner_workspace_result'  => array(
            'success' => true,
            'handle'  => 'repo@hidden-run',
            'branch'  => 'agent/hidden-run',
        ),
    );
    $runner_engine_data = array(
        'status' => 'runner-workspace-status',
        'grade'  => array(
            'checks' => array(
                array( 'id' => 'runner_check', 'passed' => true, 'score' => 1, 'max_score' => 1, 'message' => 'Looks good.' ),
            ),
        ),
    );
    $runner_capture = homeboy_datamachine_agent_capture_runner_workspace(
        $runner_engine_data,
        $runner_config
    );

    if ( ! empty( $fallback_pr_input ) ) {
        fwrite( STDERR, "Did not expect runner workspace capture to open the fallback PR before artifact context is assembled.\n" );
        exit( 1 );
    }
    $runner_template_values = homeboy_datamachine_agent_artifact_pr_context(
        43,
        $runner_config,
        $runner_engine_data,
        array(),
        homeboy_datamachine_agent_runner_workspace_written_paths( $runner_capture ),
        array(
            'success_status'           => 'completion_outcome_satisfied',
            'transcript_artifacts'     => array( 'json' => 'artifacts/runner-transcript.json' ),
            'runner_workspace_capture' => $runner_capture,
        )
    );
    $runner_capture_config = $runner_config;
    $runner_capture_config['fallback_pull_request'] = homeboy_datamachine_agent_runner_workspace_fallback_config(
        $runner_config,
        $runner_config['runner_workspace_result'],
        $runner_template_values
    );
    $runner_fallback = homeboy_datamachine_agent_open_fallback_pr( $runner_engine_data, $runner_capture_config );

    if ( empty( $runner_capture['changed'] ) || empty( $runner_fallback['opened'] ) ) {
        fwrite( STDERR, "Expected hidden runner workspace capture to commit, push, and open a fallback PR after context assembly.\n" );
        exit( 1 );
    }
    if ( 'agent/hidden-run' !== ( $fallback_pr_input['head'] ?? null ) || 'owner/repo' !== ( $fallback_pr_input['repo'] ?? null ) ) {
        fwrite( STDERR, "Expected runner workspace capture fallback PR to use the captured branch.\n" );
        exit( 1 );
    }
    if ( '[runner-agent] runner-task - agent/hidden-run - completion_outcome_satisfied' !== ( $fallback_pr_input['title'] ?? '' ) ) {
        fwrite( STDERR, "Expected runner workspace fallback PR to use the final artifact PR title context.\n" );
        exit( 1 );
    }
    foreach ( array( 'Branch: agent/hidden-run', 'Handle: repo@hidden-run', 'Status: runner-workspace-status', 'Custom: custom fallback value', 'runner_check', 'https://github.com/owner/repo/actions/runs/123456', 'artifacts/runner-transcript.json', 'docs/generated.md' ) as $expected_fallback_body_fragment ) {
        if ( ! str_contains( (string) ( $fallback_pr_input['body'] ?? '' ), $expected_fallback_body_fragment ) ) {
            fwrite( STDERR, "Expected runner workspace fallback PR body to include {$expected_fallback_body_fragment}.\n" );
            exit( 1 );
        }
    }
    if ( 'repo@hidden-run' !== ( $runner_capture_calls[0]['input']['name'] ?? null ) || array( 'docs/generated.md' ) !== ( $runner_capture_calls[1]['input']['paths'] ?? null ) ) {
        fwrite( STDERR, "Expected runner workspace capture to inspect and stage the provisioned handle.\n" );
        exit( 1 );
    }

    $fallback_pr_input = array();
    $explicit_runner_config = array(
        'target_repo'              => 'owner/repo',
        'tool_results_key'         => 'github_tool_results',
        'fallback_pull_request'    => array(
            'title' => 'Explicit fallback title',
            'body'  => 'Explicit fallback body',
        ),
        'artifact_export'          => array(
            'pr_title_template' => 'Templated {task_id}',
            'pr_body_template'  => 'Templated {workspace_branch}',
        ),
        'runner_workspace'         => array(
            'enabled'         => true,
            'expose_to_agent' => false,
        ),
        'runner_workspace_result'  => array(
            'success' => true,
            'handle'  => 'repo@explicit-run',
            'branch'  => 'agent/explicit-run',
        ),
    );
    $explicit_runner_capture = homeboy_datamachine_agent_capture_runner_workspace(
        array(),
        $explicit_runner_config
    );
    $explicit_runner_template_values = homeboy_datamachine_agent_artifact_pr_context(
        43,
        $explicit_runner_config,
        array(),
        array(),
        array(),
        array(
            'success_status'           => 'no_changes',
            'runner_workspace_capture' => $explicit_runner_capture,
        )
    );
    $explicit_runner_capture_config = $explicit_runner_config;
    $explicit_runner_capture_config['fallback_pull_request'] = homeboy_datamachine_agent_runner_workspace_fallback_config(
        $explicit_runner_config,
        $explicit_runner_config['runner_workspace_result'],
        $explicit_runner_template_values
    );
    $explicit_runner_fallback = homeboy_datamachine_agent_open_fallback_pr( array(), $explicit_runner_capture_config );
    if ( empty( $explicit_runner_fallback['opened'] ) ) {
        fwrite( STDERR, "Expected explicit runner workspace fallback PR to open.\n" );
        exit( 1 );
    }
    if ( 'Explicit fallback title' !== ( $fallback_pr_input['title'] ?? '' ) || 'Explicit fallback body' !== ( $fallback_pr_input['body'] ?? '' ) ) {
        fwrite( STDERR, "Expected explicit runner workspace fallback PR title/body to take precedence over artifact templates.\n" );
        exit( 1 );
    }

    $artifact_export_calls = array();
    $GLOBALS['homeboy_datamachine_agent_fake_abilities'] = array(
        'datamachine/create-or-update-github-file' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function ( array $input ) use ( &$artifact_export_calls ): array {
                $artifact_export_calls[] = array( 'ability' => 'file', 'input' => $input );
                return array( 'success' => true, 'html_url' => 'https://github.com/owner/repo/commit/artifact' );
            }
        ),
        'datamachine/create-github-pull-request' => new Homeboy_Datamachine_Agent_Fake_Ability(
            function ( array $input ) use ( &$artifact_export_calls ): array {
                $artifact_export_calls[] = array( 'ability' => 'pr', 'input' => $input );
                return array( 'success' => true, 'html_url' => 'https://github.com/owner/repo/pull/988' );
            }
        ),
        'datamachine/get-github-file' => new Homeboy_Datamachine_Agent_Fake_Ability(
            fn() => array( 'success' => false )
        ),
    );
    $job_artifact_export = homeboy_datamachine_agent_export_job_artifacts(
        42,
        array(
            'target_repo'     => 'owner/repo',
            'task_id'         => 'example-task',
            'task_label'      => 'Example task',
            'provider'        => 'openai',
            'model'           => 'gpt-smoke',
            'agent_slug'      => 'review-agent',
            'flow_slug'       => 'review-flow',
            'artifact_export' => array(
                'enabled'                 => true,
                'repo'                    => 'owner/repo',
                'path_prefix'             => 'bundles/task-runner',
                'include_job_artifacts'    => true,
                'branch_template'         => 'agent-artifacts/{agent_slug}-{run_id}-{job_id}',
                'commit_message_template' => 'chore: persist {type} artifact',
                'pr_title_template'       => '[{agent_slug}] {task_id} - {model_label} - {result_label}',
                'pr_body_template'        => "## Result\n{result_table}\n\nStatus: {engine_status}\nCustom: {custom_label}\n\n## Checks\n{checks_table}\n\n## Tools\n{tools_table}\n\n## Review Artifacts\n{links_table}\n",
                'pr_template_values'      => array( 'custom_label' => 'custom value' ),
                'pr_template_paths'       => array( 'engine_status' => 'engine_data.status' ),
            ),
        ),
        false,
        array(
            'status'                 => 'processing',
            'tool_execution_summary' => array(
                array( 'turn_count' => 1, 'tool_name' => 'workspace_read', 'success' => true ),
            ),
        ),
        array(
            'success_status'       => 'no_changes',
            'transcript_artifacts' => array( 'json' => 'artifacts/transcript.json' ),
            'grade'                => array(
                'checks' => array(
                    array( 'id' => 'example_check', 'passed' => false, 'score' => 0, 'max_score' => 1, 'message' => 'Needs work.' ),
                ),
            ),
        )
    );

    if ( empty( $job_artifact_export['pr_url'] ) ) {
        fwrite( STDERR, "Expected job artifact export to open a runner-owned PR.\n" );
        exit( 1 );
    }
    if ( 'bundles/task-runner/run-artifacts/review-flow/job-42/job-artifacts.json' !== ( $artifact_export_calls[0]['input']['file_path'] ?? null ) ) {
        fwrite( STDERR, "Expected Data Machine job artifact payload to be written as reviewable JSON.\n" );
        exit( 1 );
    }
    $artifact_pr_input = $artifact_export_calls[1]['input'] ?? array();
    if ( '[review-agent] example-task - openai/gpt-smoke - no_changes' !== ( $artifact_pr_input['title'] ?? '' ) ) {
        fwrite( STDERR, "Expected artifact PR title to include agent, task, model, and result.\n" );
        exit( 1 );
    }
    foreach ( array( 'Example task', 'Status: processing', 'Custom: custom value', 'example_check', 'workspace_read', 'https://github.com/owner/repo/actions/runs/123456', 'artifacts/transcript.json' ) as $expected_pr_body_fragment ) {
        if ( ! str_contains( (string) ( $artifact_pr_input['body'] ?? '' ), $expected_pr_body_fragment ) ) {
            fwrite( STDERR, "Expected artifact PR body to include {$expected_pr_body_fragment}.\n" );
            exit( 1 );
        }
    }

    $merged_daily_memory = homeboy_datamachine_agent_merge_daily_memory_artifact(
        "# Daily Memory: 2026-05-10\n\n### Existing Entry\n- Keep this.\n### Concurrent Entry\n- Do not delete this.\n",
        "# Daily Memory: 2026-05-10\n\n### Existing Entry\n- Keep this.\n### New Entry\n- Add this.\n"
    );
    foreach ( array( '### Existing Entry', '### Concurrent Entry', '### New Entry' ) as $expected_section ) {
        if ( ! str_contains( $merged_daily_memory, $expected_section ) ) {
            fwrite( STDERR, "Expected daily memory merge to preserve {$expected_section}.\n" );
            exit( 1 );
        }
    }

    $no_changes = array( 'github_tool_results' => array() );
    if ( homeboy_datamachine_agent_file_written( $no_changes, $config ) || homeboy_datamachine_agent_pr_opened( $no_changes, $config ) ) {
        fwrite( STDERR, "Expected empty tool results to remain no-changes eligible.\n" );
        exit( 1 );
    }

    $mailbox_reply = array(
        'completion_assertions_satisfied' => array(
            'complete_when_any' => array( 'mailbox_reply' ),
        ),
    );

    if ( ! homeboy_datamachine_agent_completion_outcome_satisfied( $mailbox_reply, array( 'success_completion_outcomes' => array( 'mailbox_reply' ) ) ) ) {
        fwrite( STDERR, "Expected allowed completion outcome to satisfy success.\n" );
        exit( 1 );
    }

    if ( homeboy_datamachine_agent_completion_outcome_satisfied( $mailbox_reply, array( 'success_completion_outcomes' => array( 'pr_body' ) ) ) ) {
        fwrite( STDERR, "Did not expect unlisted completion outcome to satisfy success.\n" );
        exit( 1 );
    }

    $nested_mailbox_reply = array( 'world_creator' => $mailbox_reply );
    if ( ! homeboy_datamachine_agent_completion_outcome_satisfied( $nested_mailbox_reply, array( 'engine_key' => 'world_creator', 'success_completion_outcomes' => array( 'mailbox_reply' ) ) ) ) {
        fwrite( STDERR, "Expected nested completion outcome to satisfy success.\n" );
        exit( 1 );
    }

    putenv( 'GITHUB_TOKEN=repository-token' );
    putenv( 'HOMEBOY_GITHUB_APP_TOKEN=app-token' );
    homeboy_datamachine_agent_configure_settings(
        array(
            'provider'                    => 'openai',
            'model'                       => 'gpt-5.5',
            'github_token_env'            => 'HOMEBOY_GITHUB_APP_TOKEN',
            'github_repository_token_env' => 'GITHUB_TOKEN',
            'github_profile_id'           => 'smoke-ci',
            'target_repo'                 => 'owner/repo',
            'allowed_repos'               => array( 'owner/repo', 'owner/other' ),
        )
    );
    $datamachine_settings = $GLOBALS['homeboy_datamachine_agent_fake_options']['datamachine_settings'] ?? array();
    $github_profiles = $datamachine_settings['github_credential_profiles'] ?? array();
    if ( 'repository-token' !== ( $github_profiles[0]['pat'] ?? '' ) || array( 'owner/repo' ) !== ( $github_profiles[0]['allowed_repos'] ?? array() ) ) {
        fwrite( STDERR, "Expected repository token profile to be first and scoped to the target repo.\n" );
        exit( 1 );
    }
    if ( 'app-token' !== ( $github_profiles[1]['pat'] ?? '' ) || array( 'owner/repo', 'owner/other' ) !== ( $github_profiles[1]['allowed_repos'] ?? array() ) ) {
        fwrite( STDERR, "Expected app token profile to preserve cross-repo allowed repositories.\n" );
        exit( 1 );
    }

    fwrite( STDOUT, "Data Machine agent write-without-PR smoke passed.\n" );
}
