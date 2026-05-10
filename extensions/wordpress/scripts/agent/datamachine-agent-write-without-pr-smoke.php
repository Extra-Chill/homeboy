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
                'url'       => 'https://github.com/owner/repo/pull/123',
            ),
        ),
    );

    if ( ! homeboy_datamachine_agent_pr_opened( $pr_opened, $config ) ) {
        fwrite( STDERR, "Expected pull-request URL to count as pr_opened.\n" );
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
