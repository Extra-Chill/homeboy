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
    class PluginSettings {}
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

    fwrite( STDOUT, "Data Machine agent write-without-PR smoke passed.\n" );
}
