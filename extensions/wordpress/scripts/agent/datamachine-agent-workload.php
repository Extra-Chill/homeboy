<?php
/**
 * Generic Data Machine agent workload for WordPress Playground runs.
 *
 * Configuration is read from HOMEBOY_DATAMACHINE_AGENT_CONFIG as JSON.
 */

use DataMachine\Core\Database\Agents\Agents;
use DataMachine\Core\Database\Chat\ConversationStoreFactory;
use DataMachine\Core\Database\Flows\Flows;
use DataMachine\Core\Database\Jobs\Jobs;
use DataMachine\Core\Database\Pipelines\Pipelines;
use DataMachine\Core\JobArtifacts;
use DataMachine\Core\PluginSettings;

if ( ! function_exists( 'homeboy_datamachine_agent_eval_artifact' ) ) {
	function homeboy_datamachine_agent_eval_artifact( array $metrics, array $metadata, ?string $error = null ): array {
		$engine_data = is_array( $metadata['engine_data'] ?? null ) ? $metadata['engine_data'] : array();
		$grade       = is_array( $metadata['grade'] ?? null ) ? $metadata['grade'] : ( is_array( $engine_data['grade'] ?? null ) ? $engine_data['grade'] : array() );
		$workspace   = is_array( $metadata['runner_workspace'] ?? null ) ? $metadata['runner_workspace'] : array();
		$capture     = is_array( $metadata['runner_workspace_capture'] ?? null ) ? $metadata['runner_workspace_capture'] : array();
		$transcript  = is_array( $metadata['transcript_artifacts'] ?? null ) ? $metadata['transcript_artifacts'] : array();
		$exports     = is_array( $metadata['job_artifact_exports'] ?? null ) ? $metadata['job_artifact_exports'] : array();
		$fingerprints = is_array( $metadata['fingerprints'] ?? null ) ? $metadata['fingerprints'] : array();
		$tool_audit_events = is_array( $metadata['tool_audit_events'] ?? null ) ? $metadata['tool_audit_events'] : array();
		$policy_attestation = is_array( $metadata['datamachine_code_policy_attestation'] ?? null ) ? $metadata['datamachine_code_policy_attestation'] : array();
		$provenance = is_array( $metadata['datamachine_provenance'] ?? null ) ? $metadata['datamachine_provenance'] : array();

		$failure_reasons = array();
		if ( is_array( $metadata['failure_reasons'] ?? null ) ) {
			$failure_reasons = array_values( array_filter( array_map( 'strval', $metadata['failure_reasons'] ) ) );
		}
        if ( null !== $error && empty( $failure_reasons ) ) {
            $failure_reasons[] = 'runner_error';
        }

        $prompt = (string) ( $metadata['prompt'] ?? '' );

		return array(
			'schema_version'  => 1,
			'schema_name'     => 'homeboy.agent_eval_result',
			'envelope'        => array(
				'schema_name'    => 'homeboy.sealed_eval_artifact',
				'schema_version' => 1,
				'status'         => empty( $tool_audit_events ) ? 'incomplete' : 'ready_for_replay',
			),
			'run'             => array_filter(
				array(
					'job_id'         => (int) ( $metadata['job_id'] ?? 0 ),
					'job_status'     => (string) ( $metadata['job_status'] ?? '' ),
					'success_status' => (string) ( $metadata['success_status'] ?? '' ),
					'error'          => null !== $error ? $error : (string) ( $metadata['error_message'] ?? '' ),
					'workflow_run_url' => homeboy_datamachine_agent_workflow_run_url(),
				),
				static fn( $value ) => '' !== $value && 0 !== $value && null !== $value
			),
			'task'            => array_filter(
				array(
					'id'    => (string) ( $metadata['task_id'] ?? $metadata['workload_id'] ?? $metadata['flow_slug'] ?? '' ),
					'label' => (string) ( $metadata['task_label'] ?? $metadata['workload_label'] ?? '' ),
				),
				static fn( $value ) => '' !== $value
			),
			'subject'         => array_filter(
				array(
					'target_repo' => (string) ( $metadata['target_repo'] ?? '' ),
					'bundle_path' => (string) ( $metadata['bundle_path'] ?? '' ),
					'flow_slug'   => (string) ( $metadata['flow_slug'] ?? '' ),
                ),
                static fn( $value ) => '' !== $value
            ),
            'agent'           => array_filter(
                array(
                    'slug' => (string) ( $metadata['agent_slug'] ?? '' ),
                    'id'   => (int) ( $metadata['agent_id'] ?? 0 ),
                ),
                static fn( $value ) => '' !== $value && 0 !== $value
            ),
            'model'           => array_filter(
                array(
                    'provider' => (string) ( $metadata['provider'] ?? '' ),
                    'model'    => (string) ( $metadata['model'] ?? '' ),
                ),
                static fn( $value ) => '' !== $value
            ),
			'prompt'          => array_filter(
				array(
					'sha256' => '' !== $prompt ? hash( 'sha256', $prompt ) : '',
					'bytes'  => strlen( $prompt ),
				),
				static fn( $value ) => '' !== $value && 0 !== $value
			),
			'hashes'          => array_filter(
				array(
					'prompt'      => is_array( $fingerprints['prompt'] ?? null ) ? $fingerprints['prompt'] : array(),
					'bundle'      => is_array( $fingerprints['bundle'] ?? null ) ? $fingerprints['bundle'] : array(),
					'tool_policy' => is_array( $fingerprints['tool_policy'] ?? null ) ? $fingerprints['tool_policy'] : array(),
				),
				static fn( $value ) => array() !== $value
			),
			'attestation'     => array_filter(
				array(
					'datamachine_provenance'             => $provenance,
					'datamachine_code_policy_attestation' => $policy_attestation,
					'integration_seams'                  => array_values(
						array_filter(
							array(
								empty( $provenance ) ? 'datamachine_provenance' : '',
								empty( $policy_attestation ) ? 'datamachine_code_policy_attestation' : '',
							)
						)
					),
				),
				static fn( $value ) => array() !== $value
			),
			'runtime'         => array_filter(
				array(
					'job_id'                   => (int) ( $metadata['job_id'] ?? 0 ),
					'pipeline_id'              => (int) ( $metadata['pipeline_id'] ?? 0 ),
					'flow_id'                  => (int) ( $metadata['flow_id'] ?? 0 ),
                    'transcript_session_id'    => (string) ( $metadata['transcript_session_id'] ?? '' ),
                    'completion_outcome_satisfied' => (bool) ( $metadata['completion_outcome_satisfied'] ?? false ),
                ),
                static fn( $value ) => '' !== $value && 0 !== $value && false !== $value
            ),
            'workspace'       => array_filter(
                array(
                    'provisioned' => ! empty( $workspace ),
                    'captured'    => ! empty( $capture ),
                    'changed'     => ! empty( $capture['changed'] ),
                    'handle'      => (string) ( $workspace['handle'] ?? $capture['status']['handle'] ?? '' ),
                    'branch'      => (string) ( $workspace['branch'] ?? $capture['status']['branch'] ?? '' ),
                ),
                static fn( $value ) => '' !== $value && false !== $value
            ),
			'transcript'      => array_filter(
				array(
					'session_id' => (string) ( $transcript['session_id'] ?? $metadata['transcript_session_id'] ?? '' ),
					'json'       => (string) ( $transcript['json'] ?? '' ),
					'summary'    => (string) ( $transcript['summary'] ?? '' ),
				),
				static fn( $value ) => '' !== $value
			),
			'replay'          => array(
				'tool_audit_events_available' => ! empty( $tool_audit_events ),
				'tool_audit_event_count'      => count( $tool_audit_events ),
				'tool_audit_events'           => $tool_audit_events,
				'source'                      => 'agents_api_tool_audit_events',
			),
			'termination'     => array_filter(
				array(
					'state'      => (string) ( $metadata['job_status'] ?? '' ),
					'success'    => 'completed' === (string) ( $metadata['job_status'] ?? '' ),
					'truncated'  => ! empty( $metadata['truncated'] ),
					'budget'     => (string) ( $engine_data['budget'] ?? '' ),
				),
				static fn( $value ) => '' !== $value && false !== $value
			),
			'grade'           => $grade,
			'metrics'         => $metrics,
			'failure_reasons' => $failure_reasons,
            'general_rule_results' => is_array( $metadata['general_rule_results'] ?? null ) ? $metadata['general_rule_results'] : array(),
            'rules'           => array_filter(
                array(
                    'general'       => is_array( $metadata['general_rules'] ?? null ) ? $metadata['general_rules'] : array(),
                    'task_specific' => is_array( $metadata['task_rules'] ?? null ) ? $metadata['task_rules'] : array(),
                    'all'           => is_array( $metadata['rules'] ?? null ) ? $metadata['rules'] : array(),
                )
            ),
            'probes'          => is_array( $metadata['probes'] ?? null ) ? $metadata['probes'] : array(),
            'artifacts'       => array_filter(
                array(
                    'job_artifact_exports' => $exports,
                    'fallback_pull_request' => is_array( $metadata['fallback_pull_request'] ?? null ) ? $metadata['fallback_pull_request'] : array(),
                )
            ),
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_normalized_list' ) ) {
    function homeboy_datamachine_agent_normalized_list( $value ): array {
        if ( ! is_array( $value ) ) {
            return array();
        }

        return array_values(
            array_unique(
                array_filter(
                    array_map(
                        static fn( $item ) => is_scalar( $item ) ? trim( (string) $item ) : '',
                        $value
                    ),
                    static fn( string $item ) => '' !== $item
                )
            )
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_rule_result' ) ) {
    function homeboy_datamachine_agent_rule_result( string $id, string $status, string $message, array $failure_reasons = array(), array $evidence = array() ): array {
        return array_filter(
            array(
                'id'              => $id,
                'status'          => $status,
                'passed'          => 'passed' === $status,
                'message'         => $message,
                'failure_reasons' => homeboy_datamachine_agent_normalized_list( $failure_reasons ),
                'evidence'        => $evidence,
            ),
            static fn( $value ) => array() !== $value && '' !== $value
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_rule_matches_failures' ) ) {
    function homeboy_datamachine_agent_rule_matches_failures( array $failure_reasons, array $watched_reasons ): array {
        return array_values( array_intersect( $failure_reasons, $watched_reasons ) );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_asset_paths' ) ) {
    function homeboy_datamachine_agent_asset_paths( array $paths ): array {
        return array_values(
            array_filter(
                $paths,
                static function ( string $path ): bool {
                    return (bool) preg_match( '/\.(css|scss|sass|less|js|jsx|ts|tsx|mjs|cjs)$/i', $path ) || 'theme.json' === basename( $path );
                }
            )
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_evaluate_general_rules' ) ) {
    function homeboy_datamachine_agent_evaluate_general_rules( array $metadata, array $config ): array {
        $general_rules = homeboy_datamachine_agent_normalized_list( $metadata['general_rules'] ?? $config['general_rules'] ?? array() );
        if ( empty( $general_rules ) ) {
            return array();
        }

        $failure_reasons = homeboy_datamachine_agent_normalized_list( $metadata['failure_reasons'] ?? array() );
        $capture         = is_array( $metadata['runner_workspace_capture'] ?? null ) ? $metadata['runner_workspace_capture'] : array();
        $status          = is_array( $capture['status'] ?? null ) ? $capture['status'] : array();
        $changed_paths   = homeboy_datamachine_agent_normalized_list( $status['files'] ?? array() );
        $results         = array();

        foreach ( $general_rules as $rule ) {
            if ( 'wordpress_editable_blocks' === $rule ) {
                $matched = homeboy_datamachine_agent_rule_matches_failures(
                    $failure_reasons,
                    array( 'missing_block_markup', 'missing_required_blocks', 'invalid_block', 'raw_html_or_fallback_block', 'shortcode_markup' )
                );
                $results[] = empty( $matched )
                    ? homeboy_datamachine_agent_rule_result( $rule, 'passed', 'No editable-block structure failures were reported.' )
                    : homeboy_datamachine_agent_rule_result( $rule, 'failed', 'Editable-block structure failures were reported.', $matched );
                continue;
            }

            if ( 'no_raw_html_or_shortcodes' === $rule ) {
                $matched = homeboy_datamachine_agent_rule_matches_failures( $failure_reasons, array( 'raw_html_or_fallback_block', 'shortcode_markup' ) );
                $results[] = empty( $matched )
                    ? homeboy_datamachine_agent_rule_result( $rule, 'passed', 'No raw HTML, fallback block, or shortcode failures were reported.' )
                    : homeboy_datamachine_agent_rule_result( $rule, 'failed', 'Raw HTML, fallback block, or shortcode failures were reported.', $matched );
                continue;
            }

            if ( 'no_speculative_plugin_packaging' === $rule ) {
                $matched = homeboy_datamachine_agent_rule_matches_failures( $failure_reasons, array( 'speculative_plugin_packaging_metadata' ) );
                $results[] = empty( $matched )
                    ? homeboy_datamachine_agent_rule_result( $rule, 'passed', 'No speculative plugin packaging metadata failures were reported.' )
                    : homeboy_datamachine_agent_rule_result( $rule, 'failed', 'Speculative plugin packaging metadata was reported.', $matched );
                continue;
            }

            if ( 'supported_plugin_author_metadata' === $rule ) {
                $matched = homeboy_datamachine_agent_rule_matches_failures( $failure_reasons, array( 'unsupported_plugin_author' ) );
                $results[] = empty( $matched )
                    ? homeboy_datamachine_agent_rule_result( $rule, 'passed', 'No unsupported plugin author metadata failures were reported.' )
                    : homeboy_datamachine_agent_rule_result( $rule, 'failed', 'Unsupported plugin author metadata was reported.', $matched );
                continue;
            }

            if ( 'wordpress_docs_standards' === $rule ) {
                $matched = homeboy_datamachine_agent_rule_matches_failures( $failure_reasons, array( 'wordpress_docs_standards_violation', 'missing_phpdoc', 'invalid_phpdoc_format' ) );
                $results[] = empty( $matched )
                    ? homeboy_datamachine_agent_rule_result( $rule, 'not_evaluated', 'No WordPress docs-standards evidence was attached to this run.' )
                    : homeboy_datamachine_agent_rule_result( $rule, 'failed', 'WordPress docs-standards failures were reported.', $matched );
                continue;
            }

            if ( 'production_build_when_assets_change' === $rule ) {
                $asset_paths = homeboy_datamachine_agent_asset_paths( $changed_paths );
                if ( empty( $asset_paths ) ) {
                    $results[] = homeboy_datamachine_agent_rule_result( $rule, 'passed', 'No buildable asset paths changed.', array(), array( 'changed_asset_paths' => array() ) );
                } else {
                    $results[] = homeboy_datamachine_agent_rule_result(
                        $rule,
                        'failed',
                        'Buildable asset paths changed, but no production build evidence was attached to this run.',
                        array( 'production_build_not_run' ),
                        array( 'changed_asset_paths' => $asset_paths )
                    );
                }
                continue;
            }

            $results[] = homeboy_datamachine_agent_rule_result( $rule, 'not_evaluated', 'No executable evaluator is registered for this general rule.' );
        }

        return $results;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_result' ) ) {
    function homeboy_datamachine_agent_result( array $metrics, array $metadata, ?string $error = null ): array {
        if ( null !== $error ) {
            $metadata['error'] = $error;
        }

        if ( ! isset( $metadata['eval_artifact'] ) ) {
            $metadata['eval_artifact'] = homeboy_datamachine_agent_eval_artifact( $metrics, $metadata, $error );
        }

        return array(
            'metrics'  => $metrics,
            'metadata' => $metadata,
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runtime_versions' ) ) {
    function homeboy_datamachine_agent_runtime_versions(): array {
        return array_filter(
            array(
                'php'       => PHP_VERSION,
                'wordpress' => function_exists( 'get_bloginfo' ) ? (string) get_bloginfo( 'version' ) : '',
            ),
            static fn( $value ) => '' !== $value
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_config' ) ) {
    function homeboy_datamachine_agent_config(): array {
        $raw = trim( (string) getenv( 'HOMEBOY_DATAMACHINE_AGENT_CONFIG' ) );
        if ( '' === $raw ) {
            return array();
        }

        $config = json_decode( $raw, true );
        return is_array( $config ) ? $config : array();
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_scalar' ) ) {
    function homeboy_datamachine_agent_scalar( array $config, string $key, string $default = '' ): string {
        $value = $config[ $key ] ?? $default;
        return is_scalar( $value ) ? trim( (string) $value ) : $default;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_fingerprints' ) ) {
    function homeboy_datamachine_agent_stable_value( $value ) {
        if ( ! is_array( $value ) ) {
            return $value;
        }

        $keys = array_keys( $value );
        if ( $keys !== range( 0, count( $value ) - 1 ) ) {
            ksort( $value );
        }

        foreach ( $value as $key => $child ) {
            $value[ $key ] = homeboy_datamachine_agent_stable_value( $child );
        }

        return $value;
    }

    function homeboy_datamachine_agent_json_sha256( $value ): string {
        return hash( 'sha256', wp_json_encode( homeboy_datamachine_agent_stable_value( $value ), JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE ) );
    }

    function homeboy_datamachine_agent_prompt_fingerprint( string $prompt ): array {
        return array(
            'sha256' => '' !== $prompt ? hash( 'sha256', $prompt ) : '',
            'bytes'  => strlen( $prompt ),
        );
    }

    function homeboy_datamachine_agent_bundle_fingerprint( string $bundle_path, array $config ): array {
        $fingerprint = array_filter(
            array(
                'path'         => $bundle_path,
                'repo'         => homeboy_datamachine_agent_scalar( $config, 'bundle_repo' ),
                'ref'          => homeboy_datamachine_agent_scalar( $config, 'bundle_ref' ),
                'path_in_repo' => homeboy_datamachine_agent_scalar( $config, 'bundle_path_in_repo' ),
            ),
            static fn( $value ) => '' !== $value
        );

        if ( '' === $bundle_path || ! is_dir( $bundle_path ) ) {
            return $fingerprint;
        }

        $files = array();
        $hash  = hash_init( 'sha256' );
        $bytes = 0;

        $iterator = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator( $bundle_path, FilesystemIterator::SKIP_DOTS )
        );
        foreach ( $iterator as $file ) {
            if ( ! $file instanceof SplFileInfo || ! $file->isFile() ) {
                continue;
            }

            $path = $file->getPathname();
            if ( preg_match( '#/(?:\.git|node_modules|vendor)/#', $path ) ) {
                continue;
            }
            if ( ! preg_match( '/\.(?:json|md|txt|php)$/i', $path ) ) {
                continue;
            }

            $relative = ltrim( str_replace( '\\', '/', substr( $path, strlen( rtrim( $bundle_path, DIRECTORY_SEPARATOR ) ) ) ), '/' );
            $content  = file_get_contents( $path );
            if ( false === $content ) {
                continue;
            }

            $file_hash = hash( 'sha256', $content );
            $file_size = strlen( $content );
            $files[]   = array(
                'path'   => $relative,
                'sha256' => $file_hash,
                'bytes'  => $file_size,
            );
            $bytes    += $file_size;
        }

        usort( $files, static fn( array $a, array $b ): int => strcmp( $a['path'], $b['path'] ) );
        foreach ( $files as $file ) {
            hash_update( $hash, $file['path'] . "\0" . $file['sha256'] . "\0" . $file['bytes'] . "\0" );
        }

        $fingerprint['sha256']     = hash_final( $hash );
        $fingerprint['file_count'] = count( $files );
        $fingerprint['bytes']      = $bytes;
        $fingerprint['files']      = array_slice( $files, 0, 200 );

        return $fingerprint;
    }

    function homeboy_datamachine_agent_tool_policy_fingerprint( array $config ): array {
        $policy = array(
            'required_abilities'     => $config['required_abilities'] ?? array(),
            'ability_tools'          => $config['ability_tools'] ?? array(),
            'tool_recorders'         => $config['tool_recorders'] ?? array(),
            'pipeline_step_patches'  => $config['pipeline_step_patches'] ?? array(),
            'flow_step_patches'      => $config['flow_step_patches'] ?? array(),
            'runner_workspace'       => $config['runner_workspace'] ?? array(),
            'success_requires_pr'    => ! empty( $config['success_requires_pr'] ),
            'success_completion_outcomes' => $config['success_completion_outcomes'] ?? array(),
        );

        return array(
            'sha256' => homeboy_datamachine_agent_json_sha256( $policy ),
            'policy' => homeboy_datamachine_agent_stable_value( $policy ),
        );
    }

    function homeboy_datamachine_agent_fingerprints( array $config, string $prompt, string $bundle_path ): array {
        return array(
            'prompt'     => homeboy_datamachine_agent_prompt_fingerprint( $prompt ),
            'bundle'     => homeboy_datamachine_agent_bundle_fingerprint( $bundle_path, $config ),
            'tool_policy' => homeboy_datamachine_agent_tool_policy_fingerprint( $config ),
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_path_value' ) ) {
    function homeboy_datamachine_agent_path_value( array $sources, string $path ) {
        $parts = array_filter( explode( '.', $path ), static fn( $part ) => '' !== $part );
        if ( empty( $parts ) ) {
            return null;
        }

        $value = $sources;
        foreach ( $parts as $part ) {
            if ( ! is_array( $value ) || ! array_key_exists( $part, $value ) ) {
                return null;
            }
            $value = $value[ $part ];
        }

        return $value;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_run_prompt_queue' ) ) {
    function homeboy_datamachine_agent_run_prompt_queue( array $step_config, string $prompt ): array {
        $existing_prompts = array();
        foreach ( $step_config['prompt_queue'] ?? array() as $queued_prompt ) {
            $existing_prompt = is_array( $queued_prompt ) ? trim( (string) ( $queued_prompt['prompt'] ?? '' ) ) : '';
            if ( '' !== $existing_prompt ) {
                $existing_prompts[] = $existing_prompt;
            }
        }

        $run_prompt = trim( $prompt );
        if ( ! empty( $existing_prompts ) && '' !== $run_prompt ) {
            $run_prompt = implode( "\n\n", $existing_prompts ) . "\n\nRun context:\n" . $run_prompt;
        } elseif ( ! empty( $existing_prompts ) ) {
            $run_prompt = implode( "\n\n", $existing_prompts );
        }

        return array(
            array(
                'prompt'   => $run_prompt,
                'added_at' => gmdate( 'c' ),
            ),
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_first_url' ) ) {
    function homeboy_datamachine_agent_first_url( $value ): string {
        if ( is_string( $value ) ) {
            return preg_match( '#https://github\.com/[^\s)]+#', $value, $matches ) ? $matches[0] : '';
        }
        if ( ! is_array( $value ) ) {
            return '';
        }
        foreach ( array( 'html_url', 'issue_url', 'url' ) as $key ) {
            if ( ! empty( $value[ $key ] ) && is_string( $value[ $key ] ) && str_starts_with( $value[ $key ], 'https://github.com/' ) ) {
                return $value[ $key ];
            }
        }
        foreach ( $value as $child ) {
            $url = homeboy_datamachine_agent_first_url( $child );
            if ( '' !== $url ) {
                return $url;
            }
        }
        return '';
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_pr_opened' ) ) {
    function homeboy_datamachine_agent_merge_recorded_tool_results( array $engine_data, array $config ): array {
        if ( ! class_exists( 'Homeboy_Datamachine_Agent_Tool_Recorder' ) ) {
            return $engine_data;
        }

        $recorded = Homeboy_Datamachine_Agent_Tool_Recorder::tool_results();
        if ( empty( $recorded ) ) {
            return $engine_data;
        }

        $tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
        $engine_key       = homeboy_datamachine_agent_scalar( $config, 'engine_key' );

        if ( '' !== $engine_key ) {
            if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
                $engine_data[ $engine_key ] = array();
            }
            $existing = is_array( $engine_data[ $engine_key ][ $tool_results_key ] ?? null ) ? $engine_data[ $engine_key ][ $tool_results_key ] : array();
            $engine_data[ $engine_key ][ $tool_results_key ] = array_merge( $existing, $recorded );
            return $engine_data;
        }

        $existing = is_array( $engine_data[ $tool_results_key ] ?? null ) ? $engine_data[ $tool_results_key ] : array();
        $engine_data[ $tool_results_key ] = array_merge( $existing, $recorded );
        return $engine_data;
    }

    function homeboy_datamachine_agent_tool_results( array $engine_data, array $config ): array {
        $tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
        $engine_key       = homeboy_datamachine_agent_scalar( $config, 'engine_key' );

        if ( '' !== $engine_key && is_array( $engine_data[ $engine_key ][ $tool_results_key ] ?? null ) ) {
            return $engine_data[ $engine_key ][ $tool_results_key ];
        }

        return is_array( $engine_data[ $tool_results_key ] ?? null ) ? $engine_data[ $tool_results_key ] : array();
    }

	function homeboy_datamachine_agent_set_tool_results( array $engine_data, array $config, array $tool_results ): array {
		$tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
		$engine_key       = homeboy_datamachine_agent_scalar( $config, 'engine_key' );

        if ( '' !== $engine_key ) {
            if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
                $engine_data[ $engine_key ] = array();
            }
            $engine_data[ $engine_key ][ $tool_results_key ] = $tool_results;
            return $engine_data;
        }

		$engine_data[ $tool_results_key ] = $tool_results;
		return $engine_data;
	}

	function homeboy_datamachine_agent_tool_audit_events( array $engine_data, array $config ): array {
		$engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
		$sources    = array( $engine_data );
		if ( '' !== $engine_key && is_array( $engine_data[ $engine_key ] ?? null ) ) {
			$sources[] = $engine_data[ $engine_key ];
		}

		$events = array();
		foreach ( $sources as $source ) {
			if ( ! is_array( $source['tool_audit_events'] ?? null ) ) {
				continue;
			}
			foreach ( $source['tool_audit_events'] as $event ) {
				if ( ! is_array( $event ) ) {
					continue;
				}
				$events[] = $event;
			}
		}

		return $events;
	}

	function homeboy_datamachine_agent_runner_publications( array $engine_data, array $config ): array {
		$engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
		$publications = is_array( $engine_data['runner_publications'] ?? null ) ? $engine_data['runner_publications'] : array();

		if ( '' !== $engine_key && is_array( $engine_data[ $engine_key ]['runner_publications'] ?? null ) ) {
			return array_merge( $publications, $engine_data[ $engine_key ]['runner_publications'] );
		}

		if ( '' === $engine_key ) {
			foreach ( $engine_data as $child ) {
				if ( is_array( $child ) && is_array( $child['runner_publications'] ?? null ) ) {
					$publications = array_merge( $publications, $child['runner_publications'] );
				}
			}
		}

		return $publications;
	}

	function homeboy_datamachine_agent_record_runner_publication( array $engine_data, array $config, array $publication ): array {
		$engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
		if ( '' !== $engine_key ) {
			if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
				$engine_data[ $engine_key ] = array();
			}
			if ( ! isset( $engine_data[ $engine_key ]['runner_publications'] ) || ! is_array( $engine_data[ $engine_key ]['runner_publications'] ) ) {
				$engine_data[ $engine_key ]['runner_publications'] = array();
			}
			$engine_data[ $engine_key ]['runner_publications'][] = $publication;
			return $engine_data;
		}

		if ( ! isset( $engine_data['runner_publications'] ) || ! is_array( $engine_data['runner_publications'] ) ) {
			$engine_data['runner_publications'] = array();
		}
		$engine_data['runner_publications'][] = $publication;

		return $engine_data;
	}

	function homeboy_datamachine_agent_set_tool_audit_events( array $engine_data, array $config, array $tool_audit_events ): array {
		$tool_audit_events = array_values( array_filter( $tool_audit_events, 'is_array' ) );
		if ( empty( $tool_audit_events ) ) {
			return $engine_data;
		}

		$engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
		if ( '' !== $engine_key ) {
			if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
				$engine_data[ $engine_key ] = array();
			}
			$engine_data[ $engine_key ]['tool_audit_events'] = $tool_audit_events;
			return $engine_data;
		}

		$engine_data['tool_audit_events'] = $tool_audit_events;
		return $engine_data;
	}

	function homeboy_datamachine_agent_completion_outcomes( array $engine_data, array $config ): array {
		$sources = $engine_data;
        $engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
        if ( '' !== $engine_key && is_array( $engine_data[ $engine_key ] ?? null ) ) {
            $sources = $engine_data[ $engine_key ];
        }

        $completed_outcomes = homeboy_datamachine_agent_path_value( $sources, 'completion_assertions_satisfied.complete_when_any' );
        if ( ! is_array( $completed_outcomes ) ) {
            return array();
        }

        return array_values(
            array_filter(
                array_map(
                    static fn( $outcome ) => is_scalar( $outcome ) ? trim( (string) $outcome ) : '',
                    $completed_outcomes
                ),
                static fn( string $outcome ) => '' !== $outcome
            )
        );
    }

    function homeboy_datamachine_agent_set_completion_outcomes( array $engine_data, array $config, array $completed_outcomes ): array {
        $completed_outcomes = array_values( array_unique( $completed_outcomes ) );
        if ( empty( $completed_outcomes ) ) {
            return $engine_data;
        }

        $engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
        if ( '' !== $engine_key ) {
            if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
                $engine_data[ $engine_key ] = array();
            }
            if ( ! isset( $engine_data[ $engine_key ]['completion_assertions_satisfied'] ) || ! is_array( $engine_data[ $engine_key ]['completion_assertions_satisfied'] ) ) {
                $engine_data[ $engine_key ]['completion_assertions_satisfied'] = array();
            }
            $engine_data[ $engine_key ]['completion_assertions_satisfied']['complete_when_any'] = $completed_outcomes;
            return $engine_data;
        }

        if ( ! isset( $engine_data['completion_assertions_satisfied'] ) || ! is_array( $engine_data['completion_assertions_satisfied'] ) ) {
            $engine_data['completion_assertions_satisfied'] = array();
        }
        $engine_data['completion_assertions_satisfied']['complete_when_any'] = $completed_outcomes;
        return $engine_data;
    }

    function homeboy_datamachine_agent_merge_child_engine_data( array $engine_data, array $child_jobs, array $config ): array {
        if ( empty( $child_jobs ) ) {
            return $engine_data;
        }

		$tool_results = homeboy_datamachine_agent_tool_results( $engine_data, $config );
		$tool_audit_events = homeboy_datamachine_agent_tool_audit_events( $engine_data, $config );
		$completed_outcomes = homeboy_datamachine_agent_completion_outcomes( $engine_data, $config );
		$child_summaries = array();

        foreach ( $child_jobs as $child_job ) {
            if ( ! is_array( $child_job ) ) {
                continue;
            }

            $child_engine_data = is_array( $child_job['engine_data'] ?? null ) ? $child_job['engine_data'] : array();
			$child_tool_results = homeboy_datamachine_agent_tool_results( $child_engine_data, $config );
			if ( ! empty( $child_tool_results ) ) {
				$tool_results = array_merge( $tool_results, $child_tool_results );
			}
			$child_tool_audit_events = homeboy_datamachine_agent_tool_audit_events( $child_engine_data, $config );
			if ( ! empty( $child_tool_audit_events ) ) {
				$tool_audit_events = array_merge( $tool_audit_events, $child_tool_audit_events );
			}
			$child_completion_outcomes = homeboy_datamachine_agent_completion_outcomes( $child_engine_data, $config );
			if ( ! empty( $child_completion_outcomes ) ) {
				$completed_outcomes = array_merge( $completed_outcomes, $child_completion_outcomes );
            }

            $child_summaries[] = array(
                'job_id'        => (int) ( $child_job['job_id'] ?? 0 ),
                'status'        => (string) ( $child_job['status'] ?? '' ),
                'parent_job_id' => (int) ( $child_job['parent_job_id'] ?? 0 ),
            );
        }

		$engine_data = homeboy_datamachine_agent_set_tool_results( $engine_data, $config, $tool_results );
		$engine_data = homeboy_datamachine_agent_set_tool_audit_events( $engine_data, $config, $tool_audit_events );
		$engine_data = homeboy_datamachine_agent_set_completion_outcomes( $engine_data, $config, $completed_outcomes );
        $engine_data['child_jobs'] = $child_summaries;
        return $engine_data;
    }

    function homeboy_datamachine_agent_pr_opened( array $engine_data, array $config ): bool {
        $tool_results = array_merge(
            homeboy_datamachine_agent_tool_results( $engine_data, $config ),
            homeboy_datamachine_agent_runner_publications( $engine_data, $config )
        );

        foreach ( $tool_results as $tool_result ) {
            if ( ! is_array( $tool_result ) || empty( $tool_result['success'] ) ) {
                continue;
            }

            $url = (string) ( $tool_result['url'] ?? '' );
            if ( str_contains( $url, '/pull/' ) ) {
                return true;
            }
        }

        return false;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_file_written' ) ) {
    function homeboy_datamachine_agent_file_written( array $engine_data, array $config ): bool {
        $tool_results = homeboy_datamachine_agent_tool_results( $engine_data, $config );

        foreach ( $tool_results as $tool_result ) {
            if ( ! is_array( $tool_result ) || empty( $tool_result['success'] ) ) {
                continue;
            }

            $write_tools = array(
                'create_or_update_github_file',
                'workspace_write',
                'workspace_edit',
                'workspace_apply_patch',
                'workspace_delete',
                'workspace_git_commit',
                'workspace_git_push',
            );
            if ( in_array( (string) ( $tool_result['tool_name'] ?? '' ), $write_tools, true ) ) {
                return true;
            }

            $url = (string) ( $tool_result['url'] ?? '' );
            if ( str_contains( $url, '/commit/' ) ) {
                return true;
            }
        }

        return false;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_completion_outcome_satisfied' ) ) {
    function homeboy_datamachine_agent_completion_outcome_satisfied( array $engine_data, array $config ): bool {
        $allowed_outcomes = is_array( $config['success_completion_outcomes'] ?? null ) ? $config['success_completion_outcomes'] : array();
        $allowed_outcomes = array_values(
            array_filter(
                array_map(
                    static fn( $outcome ) => is_scalar( $outcome ) ? trim( (string) $outcome ) : '',
                    $allowed_outcomes
                ),
                static fn( string $outcome ) => '' !== $outcome
            )
        );

        if ( empty( $allowed_outcomes ) ) {
            return false;
        }

        $completed_outcomes = homeboy_datamachine_agent_completion_outcomes( $engine_data, $config );

        foreach ( $completed_outcomes as $outcome ) {
            if ( is_scalar( $outcome ) && in_array( trim( (string) $outcome ), $allowed_outcomes, true ) ) {
                return true;
            }
        }

        return false;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_bundle_path_in_repo' ) ) {
    function homeboy_datamachine_agent_bundle_path_in_repo( array $config ): string {
        $configured = trim( (string) ( $config['bundle_path_in_repo'] ?? '' ), '/' );
        if ( '' !== $configured ) {
            return $configured;
        }

        $component_path = rtrim( homeboy_datamachine_agent_scalar( $config, 'component_path' ), '/' ) . '/';
        $bundle_path    = homeboy_datamachine_agent_scalar( $config, 'bundle_path' );
        if ( '' !== $component_path && '' !== $bundle_path && str_starts_with( $bundle_path, $component_path ) ) {
            return trim( substr( $bundle_path, strlen( $component_path ) ), '/' );
        }

        return '';
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_template' ) ) {
    function homeboy_datamachine_agent_template( string $template, array $values ): string {
        $replacements = array();
        foreach ( $values as $key => $value ) {
            $replacements[ '{' . $key . '}' ] = (string) $value;
        }

        return strtr( $template, $replacements );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_slug_fragment' ) ) {
    function homeboy_datamachine_agent_slug_fragment( string $value ): string {
        $fragment = strtolower( preg_replace( '/[^a-zA-Z0-9._-]+/', '-', $value ) ?? '' );
        $fragment = trim( $fragment, '.-' );
        return '' !== $fragment ? $fragment : 'artifact';
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_markdown_cell' ) ) {
    function homeboy_datamachine_agent_markdown_cell( $value ): string {
        if ( is_bool( $value ) ) {
            $value = $value ? 'yes' : 'no';
        } elseif ( is_array( $value ) || is_object( $value ) ) {
            $value = wp_json_encode( $value, JSON_UNESCAPED_SLASHES );
        }

        $value = trim( str_replace( array( "\r", "\n", '|' ), array( '', '<br>', '\\|' ), (string) $value ) );
        return '' !== $value ? $value : '-';
    }

    function homeboy_datamachine_agent_markdown_table( array $headers, array $rows ): string {
        if ( empty( $rows ) ) {
            return '_None recorded._';
        }

        $header = '| ' . implode( ' | ', array_map( 'homeboy_datamachine_agent_markdown_cell', $headers ) ) . ' |';
        $rule   = '| ' . implode( ' | ', array_fill( 0, count( $headers ), '---' ) ) . ' |';
        $lines  = array( $header, $rule );
        foreach ( $rows as $row ) {
            $cells = array();
            foreach ( array_keys( $headers ) as $index ) {
                $cells[] = homeboy_datamachine_agent_markdown_cell( is_array( $row ) ? ( $row[ $index ] ?? '' ) : '' );
            }
            $lines[] = '| ' . implode( ' | ', $cells ) . ' |';
        }

        return implode( "\n", $lines );
    }

    function homeboy_datamachine_agent_source_value( array $sources, string $path ) {
        $parts = array_filter( explode( '.', $path ), static fn( $part ) => '' !== $part );
        if ( empty( $parts ) ) {
            return null;
        }
        $source = array_shift( $parts );
        $value  = $sources[ $source ] ?? null;
        foreach ( $parts as $part ) {
            if ( ! is_array( $value ) || ! array_key_exists( $part, $value ) ) {
                return null;
            }
            $value = $value[ $part ];
        }
        return $value;
    }

    function homeboy_datamachine_agent_workflow_run_url(): string {
        $repository = trim( (string) getenv( 'GITHUB_REPOSITORY' ) );
        $run_id     = trim( (string) getenv( 'GITHUB_RUN_ID' ) );
        if ( '' === $repository || '' === $run_id ) {
            return '';
        }
        $server = trim( (string) getenv( 'GITHUB_SERVER_URL' ) );
        if ( '' === $server ) {
            $server = 'https://github.com';
        }
        return rtrim( $server, '/' ) . '/' . $repository . '/actions/runs/' . rawurlencode( $run_id );
    }

    function homeboy_datamachine_agent_artifact_pr_context( int $job_id, array $config, array $engine_data, array $artifact_result, array $written, array $run_context ): array {
        $workflow_url      = homeboy_datamachine_agent_workflow_run_url();
        $task_id           = homeboy_datamachine_agent_scalar( $config, 'task_id', homeboy_datamachine_agent_scalar( $config, 'workload_id', homeboy_datamachine_agent_scalar( $config, 'flow_slug', 'task' ) ) );
        $task_label        = homeboy_datamachine_agent_scalar( $config, 'task_label', homeboy_datamachine_agent_scalar( $config, 'workload_label', $task_id ) );
        $provider          = homeboy_datamachine_agent_scalar( $config, 'provider', 'provider' );
        $model             = homeboy_datamachine_agent_scalar( $config, 'model', 'model' );
        $agent_slug        = homeboy_datamachine_agent_scalar( $config, 'agent_slug', 'agent' );
        $error_message     = (string) ( $run_context['error_message'] ?? $engine_data['error_message'] ?? '' );
        $success_status    = (string) ( $run_context['success_status'] ?? '' );
        $result_label      = '' !== $success_status ? $success_status : ( '' !== $error_message ? 'failed' : 'artifact' );
        $runner_workspace  = is_array( $run_context['runner_workspace_capture']['status'] ?? null ) ? $run_context['runner_workspace_capture']['status'] : array();
        $transcript        = is_array( $run_context['transcript_artifacts'] ?? null ) ? $run_context['transcript_artifacts'] : array();
        $grade             = is_array( $run_context['grade'] ?? null ) ? $run_context['grade'] : ( is_array( $engine_data['grade'] ?? null ) ? $engine_data['grade'] : array() );

        $result_rows = array(
            array( 'Task', $task_label ),
            array( 'Task ID', $task_id ),
            array( 'Agent', $agent_slug ),
            array( 'Model', $provider . ' / ' . $model ),
            array( 'Job', $job_id ),
            array( 'Result', $result_label ),
        );
        if ( '' !== $error_message ) {
            $result_rows[] = array( 'Error', $error_message );
        }

        $check_rows = array();
        foreach ( (array) ( $grade['checks'] ?? array() ) as $check ) {
            if ( is_array( $check ) ) {
                $check_rows[] = array(
                    (string) ( $check['id'] ?? '' ),
                    ! empty( $check['passed'] ),
                    (string) ( $check['score'] ?? '' ),
                    (string) ( $check['max_score'] ?? '' ),
                    (string) ( $check['message'] ?? '' ),
                );
            }
        }

        $tool_rows = array();
        foreach ( (array) ( $engine_data['tool_execution_summary'] ?? array() ) as $tool ) {
            if ( is_array( $tool ) ) {
                $tool_rows[] = array(
                    (string) ( $tool['turn_count'] ?? '' ),
                    (string) ( $tool['tool_name'] ?? '' ),
                    ! empty( $tool['success'] ),
                );
            }
        }

        $link_rows = array();
        if ( '' !== $workflow_url ) {
            $link_rows[] = array( 'Workflow run', $workflow_url );
        }
        foreach ( array( 'json' => 'Transcript JSON', 'summary' => 'Transcript summary' ) as $key => $label ) {
            if ( ! empty( $transcript[ $key ] ) && is_string( $transcript[ $key ] ) ) {
                $link_rows[] = array( $label, '`' . $transcript[ $key ] . '`' );
            }
        }
        foreach ( $written as $path ) {
            $link_rows[] = array( 'Artifact', '`' . $path . '`' );
        }

        $values = array(
            'task_id'           => $task_id,
            'task_label'        => $task_label,
            'agent_slug'        => $agent_slug,
            'provider'          => $provider,
            'model'             => $model,
            'model_label'       => $provider . '/' . $model,
            'job_id'            => $job_id,
            'result_label'      => $result_label,
            'error_message'     => $error_message,
            'workflow_run_url'  => $workflow_url,
            'workspace_branch'  => (string) ( $runner_workspace['branch'] ?? '' ),
            'workspace_handle'  => (string) ( $runner_workspace['handle'] ?? $runner_workspace['name'] ?? '' ),
            'workspace_changed' => ! empty( $run_context['runner_workspace_capture']['changed'] ) ? 'yes' : 'no',
            'result_table'      => homeboy_datamachine_agent_markdown_table( array( 'Field', 'Value' ), $result_rows ),
            'checks_table'      => homeboy_datamachine_agent_markdown_table( array( 'Check', 'Passed', 'Score', 'Max', 'Message' ), $check_rows ),
            'tools_table'       => homeboy_datamachine_agent_markdown_table( array( 'Turn', 'Tool', 'Success' ), $tool_rows ),
            'links_table'       => homeboy_datamachine_agent_markdown_table( array( 'Artifact', 'Location' ), $link_rows ),
            'paths'             => '- `' . implode( "`\n- `", $written ) . '`',
        );

        $export_config = is_array( $config['artifact_export'] ?? null ) ? $config['artifact_export'] : array();
        foreach ( (array) ( $export_config['pr_template_values'] ?? array() ) as $key => $value ) {
            if ( is_string( $key ) && '' !== $key && is_scalar( $value ) ) {
                $values[ $key ] = (string) $value;
            }
        }

        $sources = array(
            'config'          => $config,
            'engine_data'     => $engine_data,
            'artifact_result' => $artifact_result,
            'run'             => $run_context,
            'env'             => array(
                'GITHUB_RUN_ID'      => (string) getenv( 'GITHUB_RUN_ID' ),
                'GITHUB_RUN_ATTEMPT' => (string) getenv( 'GITHUB_RUN_ATTEMPT' ),
                'GITHUB_REPOSITORY'  => (string) getenv( 'GITHUB_REPOSITORY' ),
            ),
        );
        foreach ( (array) ( $export_config['pr_template_paths'] ?? array() ) as $key => $path ) {
            if ( ! is_string( $key ) || '' === $key || ! is_string( $path ) || '' === $path ) {
                continue;
            }
            $value = homeboy_datamachine_agent_source_value( $sources, $path );
            if ( is_scalar( $value ) ) {
                $values[ $key ] = (string) $value;
            }
        }

        return $values;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_exportable_artifacts' ) ) {
    function homeboy_datamachine_agent_job_artifact_relative_path( int $job_id, array $config ): string {
        $flow_slug = homeboy_datamachine_agent_slug_fragment( homeboy_datamachine_agent_scalar( $config, 'flow_slug', 'run' ) );
        return sprintf( 'run-artifacts/%s/job-%d/job-artifacts.json', $flow_slug, $job_id );
    }

    function homeboy_datamachine_agent_export_includes_job_artifacts( array $config ): bool {
        $export_config = is_array( $config['artifact_export'] ?? null ) ? $config['artifact_export'] : array();
        return filter_var( $export_config['include_job_artifacts'] ?? false, FILTER_VALIDATE_BOOLEAN );
    }

    function homeboy_datamachine_agent_exportable_artifacts( array $artifacts ): array {
        $exportable_artifacts = array();

        foreach ( $artifacts as $artifact_group ) {
            if ( ! is_array( $artifact_group ) ) {
                continue;
            }

            foreach ( $artifact_group as $artifact ) {
                if ( ! is_array( $artifact ) ) {
                    continue;
                }

                $relative_path = trim( (string) ( $artifact['bundle_relative_path'] ?? '' ), '/' );
                $content       = (string) ( $artifact['content'] ?? '' );
                if ( '' === $relative_path || '' === $content || str_contains( $relative_path, '..' ) ) {
                    continue;
                }

                $exportable_artifacts[] = array(
                    'type'          => (string) ( $artifact['type'] ?? 'artifact' ),
                    'relative_path' => $relative_path,
                    'content'       => $content,
                );
            }
        }

        return $exportable_artifacts;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_merge_daily_memory_artifact' ) ) {
    function homeboy_datamachine_agent_merge_daily_memory_artifact( string $existing_content, string $artifact_content ): string {
        $existing_content = rtrim( $existing_content );
        $artifact_content = rtrim( $artifact_content );
        if ( '' === $existing_content || str_starts_with( $artifact_content, $existing_content ) ) {
            return $artifact_content . "\n";
        }

        preg_match_all( '/^### .*(?:\n(?!### ).*)*/m', $artifact_content, $matches );
        $merged = $existing_content;
        foreach ( $matches[0] ?? array() as $section ) {
            $section = trim( (string) $section );
            if ( '' !== $section && ! str_contains( $existing_content, $section ) ) {
                $merged .= "\n" . $section;
            }
        }

        return rtrim( $merged ) . "\n";
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_pr_head_branch' ) ) {
    function homeboy_datamachine_agent_pr_head_branch( array $engine_data, array $config ): string {
        $publication_results = array_merge(
            homeboy_datamachine_agent_tool_results( $engine_data, $config ),
            homeboy_datamachine_agent_runner_publications( $engine_data, $config )
        );

        foreach ( $publication_results as $tool_result ) {
            if ( ! is_array( $tool_result ) || empty( $tool_result['success'] ) || 'create_github_pull_request' !== (string) ( $tool_result['tool_name'] ?? '' ) ) {
                continue;
            }

            $head = trim( (string) ( $tool_result['head'] ?? '' ) );
            if ( '' !== $head ) {
                return $head;
            }
        }

        return '';
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_artifact_engine_key' ) ) {
    function homeboy_datamachine_agent_artifact_engine_key( array $config ): string {
        $engine_key = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
        if ( '' !== $engine_key ) {
            return $engine_key;
        }

        $agent_slug = homeboy_datamachine_agent_slug_fragment( homeboy_datamachine_agent_scalar( $config, 'agent_slug', 'agent' ) );
        return str_replace( array( '-', '.' ), '_', $agent_slug );
    }

    function homeboy_datamachine_agent_record_artifact_pr_result( array $engine_data, array $config, string $pr_url, string $branch, array $paths ): array {
        if ( '' === $pr_url ) {
            return $engine_data;
        }

        $repo        = homeboy_datamachine_agent_scalar( $config, 'target_repo', homeboy_datamachine_agent_scalar( $config['artifact_export'] ?? array(), 'repo' ) );
        $engine_key  = homeboy_datamachine_agent_artifact_engine_key( $config );
        $publication_config = $config;
        if ( '' !== $engine_key ) {
            $publication_config['engine_key'] = $engine_key;
        }

        $engine_data = homeboy_datamachine_agent_record_runner_publication(
            $engine_data,
            $publication_config,
            array(
                'tool_name' => 'create_github_pull_request',
                'source'    => 'runner_artifact_export',
                'success'   => true,
                'repo'      => $repo,
                'head'      => $branch,
                'url'       => $pr_url,
                'result'    => array(
                    'success'  => true,
                    'html_url' => $pr_url,
                    'head'     => $branch,
                ),
            )
        );
        if ( '' === $engine_key ) {
            return $engine_data;
        }

        if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
            $engine_data[ $engine_key ] = array();
        }

        $engine_data[ $engine_key ] = array_merge(
            $engine_data[ $engine_key ],
            array(
                'success'         => true,
                'pr_url'          => $pr_url,
                'head'            => $branch,
                'artifact_export' => true,
                'artifact_paths'  => array_values( $paths ),
            )
        );

        return $engine_data;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_export_job_artifacts' ) ) {
    function homeboy_datamachine_agent_export_job_artifacts( int $job_id, array $config, bool $pr_opened, array $engine_data = array(), array $run_context = array() ): array {
        $export_config = is_array( $config['artifact_export'] ?? null ) ? $config['artifact_export'] : array();
        if ( $job_id <= 0 || empty( $export_config['enabled'] ) || ! class_exists( JobArtifacts::class ) || ! function_exists( 'wp_get_ability' ) ) {
            return array();
        }

        $target_repo = trim( (string) ( $export_config['repo'] ?? '' ) );
        $path_prefix = trim( (string) ( $export_config['path_prefix'] ?? '' ), '/' );
        if ( '' === $target_repo || '' === $path_prefix || str_contains( $path_prefix, '..' ) ) {
            return array( 'error' => 'Artifact export requires artifact_export.repo and artifact_export.path_prefix.' );
        }

        $artifact_result = ( new JobArtifacts() )->get( $job_id );
        $artifacts       = is_array( $artifact_result['artifacts'] ?? null ) ? $artifact_result['artifacts'] : array();
        if (
            ! empty( $artifact_result['success'] )
            && ! empty( $artifacts )
            && homeboy_datamachine_agent_export_includes_job_artifacts( $config )
        ) {
            $artifacts['job_artifacts'] = array(
                array(
                    'type'                 => 'job_artifacts',
                    'bundle_relative_path' => homeboy_datamachine_agent_job_artifact_relative_path( $job_id, $config ),
                    'content'              => wp_json_encode( $artifacts, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES ) . "\n",
                ),
            );
        }
        $exportable_artifacts = homeboy_datamachine_agent_exportable_artifacts( $artifacts );
        if ( empty( $exportable_artifacts ) ) {
            return array();
        }

        $file_ability = wp_get_ability( 'datamachine/create-or-update-github-file' );
        $pr_ability   = wp_get_ability( 'datamachine/create-github-pull-request' );
        $get_ability  = wp_get_ability( 'datamachine/get-github-file' );
        if ( ! $file_ability || ! $pr_ability || ! $get_ability ) {
            return array( 'error' => 'GitHub file or pull request ability unavailable.' );
        }

        $agent_slug      = homeboy_datamachine_agent_slug_fragment( homeboy_datamachine_agent_scalar( $config, 'agent_slug', 'agent' ) );
        $run_id          = homeboy_datamachine_agent_slug_fragment( (string) getenv( 'GITHUB_RUN_ID' ) );
        $template_values = array(
            'agent_slug' => $agent_slug,
            'run_id'     => $run_id,
            'job_id'     => $job_id,
            'provider'   => homeboy_datamachine_agent_slug_fragment( homeboy_datamachine_agent_scalar( $config, 'provider', 'provider' ) ),
            'model'      => homeboy_datamachine_agent_slug_fragment( homeboy_datamachine_agent_scalar( $config, 'model', 'model' ) ),
        );
        $branch_template = (string) ( $export_config['branch_template'] ?? '' );
        $attached_to_pr  = false;
        $branch          = '';
        if ( $pr_opened && ! empty( $export_config['only_when_no_pr'] ) ) {
            $branch         = homeboy_datamachine_agent_pr_head_branch( $engine_data, $config );
            $attached_to_pr = true;
            if ( '' === $branch ) {
                return array( 'error' => 'Artifact export could not identify the current run pull request branch.' );
            }
        }
        if ( '' === $branch ) {
            $branch = homeboy_datamachine_agent_template( $branch_template, $template_values );
        }
        if ( '' === $branch ) {
            return array( 'error' => 'Artifact export requires artifact_export.branch_template.' );
        }

        $written = array();

        foreach ( $exportable_artifacts as $artifact ) {
            $repo_path        = $path_prefix . '/' . $artifact['relative_path'];
            $artifact_values  = array_merge(
                $template_values,
                array(
                    'type' => $artifact['type'],
                    'path' => $repo_path,
                )
            );
            $commit_message   = homeboy_datamachine_agent_template( (string) ( $export_config['commit_message_template'] ?? '' ), $artifact_values );
            if ( '' === $commit_message ) {
                return array( 'error' => 'Artifact export requires artifact_export.commit_message_template.' );
            }

            $content = $artifact['content'];
            if ( 'agent_daily_memory' === $artifact['type'] ) {
                $existing = $get_ability->execute(
                    array(
                        'repo' => $target_repo,
                        'path' => $repo_path,
                        'ref'  => $branch,
                    )
                );
                if ( is_array( $existing ) && ! empty( $existing['success'] ) && is_array( $existing['file'] ?? null ) && is_string( $existing['file']['content'] ?? null ) ) {
                    $content = homeboy_datamachine_agent_merge_daily_memory_artifact( $existing['file']['content'], $content );
                }
            }

            $result = $file_ability->execute(
                array(
                    'repo'           => $target_repo,
                    'file_path'      => $repo_path,
                    'content'        => $content,
                    'commit_message' => $commit_message,
                    'branch'         => $branch,
                )
            );
            if ( function_exists( 'is_wp_error' ) && is_wp_error( $result ) ) {
                return array( 'error' => $result->get_error_message(), 'written' => $written );
            }
            if ( ! is_array( $result ) || empty( $result['success'] ) ) {
                return array( 'error' => (string) ( $result['error'] ?? 'Artifact file export failed.' ), 'written' => $written );
            }

            $written[] = $repo_path;
        }

        if ( empty( $written ) ) {
            return array();
        }

        if ( $attached_to_pr ) {
            return array_filter(
                array(
                    'branch'         => $branch,
                    'paths'          => $written,
                    'attached_to_pr' => true,
                )
            );
        }

        $pr_title_template = (string) ( $export_config['pr_title_template'] ?? '' );
        $pr_body_template  = (string) ( $export_config['pr_body_template'] ?? '' );
        if ( '' === $pr_title_template || '' === $pr_body_template ) {
            return array( 'error' => 'Artifact export requires artifact_export.pr_title_template and artifact_export.pr_body_template.' );
        }

        $pr_values = array_merge(
            $template_values,
            homeboy_datamachine_agent_artifact_pr_context( $job_id, $config, $engine_data, $artifact_result, $written, $run_context )
        );
        $pr_result = $pr_ability->execute(
            array(
                'repo'  => $target_repo,
                'title' => homeboy_datamachine_agent_template( $pr_title_template, $pr_values ),
                'head'  => $branch,
                'body'  => homeboy_datamachine_agent_template( $pr_body_template, $pr_values ),
            )
        );
        if ( function_exists( 'is_wp_error' ) && is_wp_error( $pr_result ) ) {
            return array( 'error' => $pr_result->get_error_message(), 'written' => $written, 'branch' => $branch );
        }

        $pr_url = '';
        if ( is_array( $pr_result ) ) {
            $pr_url = homeboy_datamachine_agent_first_url( $pr_result );
        }

        return array_filter(
            array(
                'branch'      => $branch,
                'paths'       => $written,
                'pr_url'      => $pr_url,
                'engine_data' => homeboy_datamachine_agent_record_artifact_pr_result( $engine_data, $config, $pr_url, $branch, $written ),
            )
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_open_fallback_pr' ) ) {
    function homeboy_datamachine_agent_record_pr_tool_result( array $engine_data, array $config, array $tool_result ): array {
        $tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
        $engine_key       = homeboy_datamachine_agent_scalar( $config, 'engine_key' );
        if ( '' !== $engine_key ) {
            if ( ! isset( $engine_data[ $engine_key ] ) || ! is_array( $engine_data[ $engine_key ] ) ) {
                $engine_data[ $engine_key ] = array();
            }
            if ( ! isset( $engine_data[ $engine_key ][ $tool_results_key ] ) || ! is_array( $engine_data[ $engine_key ][ $tool_results_key ] ) ) {
                $engine_data[ $engine_key ][ $tool_results_key ] = array();
            }
            $engine_data[ $engine_key ][ $tool_results_key ][] = $tool_result;
            return $engine_data;
        }

        if ( ! isset( $engine_data[ $tool_results_key ] ) || ! is_array( $engine_data[ $tool_results_key ] ) ) {
            $engine_data[ $tool_results_key ] = array();
        }
        $engine_data[ $tool_results_key ][] = $tool_result;

        return $engine_data;
    }

    function homeboy_datamachine_agent_existing_fallback_pr( string $repo, string $head, string $base ): array {
        $ability = function_exists( 'wp_get_ability' ) ? wp_get_ability( 'datamachine/list-github-pulls' ) : null;
        if ( ! $ability ) {
            return array();
        }

        $result = $ability->execute(
            array(
                'repo'     => $repo,
                'state'    => 'open',
                'per_page' => 100,
            )
        );
        if ( function_exists( 'is_wp_error' ) && is_wp_error( $result ) ) {
            return array();
        }
        if ( ! is_array( $result ) || empty( $result['success'] ) || ! is_array( $result['pulls'] ?? null ) ) {
            return array();
        }

        foreach ( $result['pulls'] as $pull ) {
            if ( ! is_array( $pull ) ) {
                continue;
            }
            if ( $head !== (string) ( $pull['head'] ?? $pull['head_ref'] ?? '' ) ) {
                continue;
            }
            if ( '' !== $base && $base !== (string) ( $pull['base'] ?? $pull['base_ref'] ?? '' ) ) {
                continue;
            }
            if ( '' !== homeboy_datamachine_agent_first_url( $pull ) ) {
                return $pull;
            }
        }

        return array();
    }

    function homeboy_datamachine_agent_open_fallback_pr( array $engine_data, array $config ): array {
        $fallback = is_array( $config['fallback_pull_request'] ?? null ) ? $config['fallback_pull_request'] : array();
        if ( empty( $fallback ) ) {
            return array( 'opened' => false );
        }

        $repo  = homeboy_datamachine_agent_scalar( $fallback, 'repo', homeboy_datamachine_agent_scalar( $config, 'target_repo' ) );
        $title = homeboy_datamachine_agent_scalar( $fallback, 'title' );
        $head  = homeboy_datamachine_agent_scalar( $fallback, 'head' );
        if ( '' === $repo || '' === $title || '' === $head ) {
            return array( 'opened' => false, 'error' => 'fallback_pull_request requires repo, title, and head.' );
        }

        $base        = homeboy_datamachine_agent_scalar( $fallback, 'base' );
        $existing_pr = homeboy_datamachine_agent_existing_fallback_pr( $repo, $head, $base );
        if ( ! empty( $existing_pr ) ) {
            $tool_result = array(
                'tool_name' => 'create_github_pull_request',
                'source'    => 'runner_fallback_pull_request',
                'success'   => true,
                'repo'      => $repo,
                'head'      => $head,
                'base'      => $base,
                'url'       => homeboy_datamachine_agent_first_url( $existing_pr ),
                'result'    => array(
                    'success'      => true,
                    'pull_request' => $existing_pr,
                    'pull_number'  => (int) ( $existing_pr['number'] ?? 0 ),
                    'html_url'     => homeboy_datamachine_agent_first_url( $existing_pr ),
                    'message'      => sprintf( 'Reused existing pull request #%d in %s.', (int) ( $existing_pr['number'] ?? 0 ), $repo ),
                ),
            );

            return array(
                'opened'      => true,
                'reused'      => true,
                'result'      => $tool_result['result'],
                'input'       => array( 'repo' => $repo, 'title' => $title, 'head' => $head, 'base' => $base ),
                'engine_data' => homeboy_datamachine_agent_record_runner_publication( $engine_data, $config, $tool_result ),
            );
        }

        $ability = function_exists( 'wp_get_ability' ) ? wp_get_ability( 'datamachine/create-github-pull-request' ) : null;
        if ( ! $ability ) {
            return array( 'opened' => false, 'error' => 'datamachine/create-github-pull-request ability is not available.' );
        }

        $input = array(
            'repo'                  => $repo,
            'title'                 => $title,
            'head'                  => $head,
            'body'                  => (string) ( $fallback['body'] ?? '' ),
            'draft'                 => ! empty( $fallback['draft'] ),
            'maintainer_can_modify' => array_key_exists( 'maintainer_can_modify', $fallback ) ? (bool) $fallback['maintainer_can_modify'] : true,
        );
        if ( '' !== $base ) {
            $input['base'] = $base;
        }

        $result = $ability->execute( $input );
        if ( is_wp_error( $result ) ) {
            return array( 'opened' => false, 'error' => $result->get_error_message(), 'input' => $input );
        }
        if ( ! is_array( $result ) || empty( $result['success'] ) ) {
            $error = is_array( $result ) ? (string) ( $result['error'] ?? 'Fallback pull request creation failed.' ) : 'Fallback pull request creation failed.';
            if ( is_array( $result ) && ! empty( $result['message'] ) && ! str_contains( $error, (string) $result['message'] ) ) {
                $error .= ': ' . (string) $result['message'];
            }
            return array( 'opened' => false, 'error' => $error, 'input' => $input, 'result' => $result );
        }

        $tool_result = array(
            'tool_name' => 'create_github_pull_request',
            'source'    => 'runner_fallback_pull_request',
            'success'   => true,
            'repo'      => $repo,
            'head'      => $head,
            'base'      => (string) ( $input['base'] ?? '' ),
            'url'       => (string) ( $result['html_url'] ?? '' ),
            'result'    => $result,
        );

        $engine_data = homeboy_datamachine_agent_record_runner_publication( $engine_data, $config, $tool_result );

        return array( 'opened' => true, 'result' => $result, 'input' => $input, 'engine_data' => $engine_data );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_config' ) ) {
    function homeboy_datamachine_agent_runner_workspace_config( array $config ): array {
        return is_array( $config['runner_workspace'] ?? null ) ? $config['runner_workspace'] : array();
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_exposed' ) ) {
    function homeboy_datamachine_agent_runner_workspace_exposed( array $config ): bool {
        return homeboy_datamachine_agent_bool_config( homeboy_datamachine_agent_runner_workspace_config( $config ), 'expose_to_agent', true );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_alias' ) ) {
    function homeboy_datamachine_agent_runner_workspace_alias( array $config ): string {
        $workspace = homeboy_datamachine_agent_runner_workspace_config( $config );
        $alias     = isset( $workspace['agent_alias'] ) && is_scalar( $workspace['agent_alias'] ) ? trim( (string) $workspace['agent_alias'] ) : '';
        return '' !== $alias ? $alias : '';
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_root' ) ) {
    function homeboy_datamachine_agent_runner_workspace_root( array $config ): string {
        $workspace = homeboy_datamachine_agent_runner_workspace_config( $config );
        $root      = isset( $workspace['agent_root'] ) && is_scalar( $workspace['agent_root'] ) ? trim( (string) $workspace['agent_root'] ) : '';
        $root      = trim( str_replace( '\\', '/', $root ), '/' );
        $parts     = array();
        foreach ( explode( '/', $root ) as $part ) {
            if ( '' === $part || '.' === $part || '..' === $part ) {
                continue;
            }
            $parts[] = $part;
        }

        return implode( '/', $parts );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_capture_enabled' ) ) {
    function homeboy_datamachine_agent_runner_workspace_capture_enabled( array $config ): bool {
        $workspace = homeboy_datamachine_agent_runner_workspace_config( $config );
        if ( ! homeboy_datamachine_agent_bool_config( $workspace, 'enabled', false ) ) {
            return false;
        }
        if ( array_key_exists( 'capture_changes', $workspace ) ) {
            return homeboy_datamachine_agent_bool_config( $workspace, 'capture_changes', false );
        }

        return ! homeboy_datamachine_agent_runner_workspace_exposed( $config );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_execute_workspace_ability' ) ) {
    function homeboy_datamachine_agent_execute_workspace_ability( string $ability_name, array $input ): array {
        $ability = function_exists( 'wp_get_ability' ) ? wp_get_ability( $ability_name ) : null;
        if ( ! $ability ) {
            return array( 'success' => false, 'error' => $ability_name . ' is not registered.' );
        }

        $result = $ability->execute( $input );
        if ( function_exists( 'is_wp_error' ) && is_wp_error( $result ) ) {
            return array( 'success' => false, 'error' => $result->get_error_message(), 'input' => $input );
        }
        if ( ! is_array( $result ) ) {
            return array( 'success' => false, 'error' => $ability_name . ' returned a non-array result.', 'input' => $input );
        }

        return $result + array( 'success' => ! empty( $result['success'] ) );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_fallback_config' ) ) {
    function homeboy_datamachine_agent_runner_workspace_fallback_config( array $config, array $runner_workspace, array $template_values = array() ): array {
        $fallback = is_array( $config['fallback_pull_request'] ?? null ) ? $config['fallback_pull_request'] : array();
        $repo     = homeboy_datamachine_agent_scalar( $fallback, 'repo', homeboy_datamachine_agent_scalar( $config, 'target_repo' ) );
        $branch   = (string) ( $runner_workspace['branch'] ?? '' );

        if ( '' === homeboy_datamachine_agent_scalar( $fallback, 'head' ) && '' !== $branch ) {
            $fallback['head'] = $branch;
        }
        if ( '' === homeboy_datamachine_agent_scalar( $fallback, 'repo' ) && '' !== $repo ) {
            $fallback['repo'] = $repo;
        }
        $export_config = is_array( $config['artifact_export'] ?? null ) ? $config['artifact_export'] : array();
        if ( ! empty( $template_values ) && '' === homeboy_datamachine_agent_scalar( $fallback, 'title' ) && '' !== (string) ( $export_config['pr_title_template'] ?? '' ) ) {
            $fallback['title'] = homeboy_datamachine_agent_template( (string) $export_config['pr_title_template'], $template_values );
        }
        if ( ! empty( $template_values ) && '' === homeboy_datamachine_agent_scalar( $fallback, 'body' ) && '' !== (string) ( $export_config['pr_body_template'] ?? '' ) ) {
            $fallback['body'] = homeboy_datamachine_agent_template( (string) $export_config['pr_body_template'], $template_values );
        }
        if ( '' === homeboy_datamachine_agent_scalar( $fallback, 'title' ) ) {
            $fallback['title'] = 'Persist Data Machine agent workspace changes';
        }

        return $fallback;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_capture_runner_workspace' ) ) {
    function homeboy_datamachine_agent_capture_runner_workspace( array $engine_data, array $config ): array {
        if ( ! homeboy_datamachine_agent_runner_workspace_capture_enabled( $config ) ) {
            return array( 'enabled' => false, 'changed' => false, 'engine_data' => $engine_data );
        }

        $runner_workspace = is_array( $config['runner_workspace_result'] ?? null ) ? $config['runner_workspace_result'] : array();
        $handle           = (string) ( $runner_workspace['handle'] ?? '' );
        if ( empty( $runner_workspace['success'] ) || '' === $handle ) {
            return array( 'enabled' => true, 'changed' => false, 'engine_data' => $engine_data, 'error' => 'Runner workspace capture requires a provisioned workspace handle.' );
        }

        $status = homeboy_datamachine_agent_execute_workspace_ability( 'datamachine/workspace-git-status', array( 'name' => $handle ) );
        if ( empty( $status['success'] ) ) {
            return array( 'enabled' => true, 'changed' => false, 'engine_data' => $engine_data, 'status' => $status, 'error' => (string) ( $status['error'] ?? 'Workspace status failed.' ) );
        }
        $status['branch'] = (string) ( $runner_workspace['branch'] ?? '' );
        $status['handle'] = $handle;

        $files = is_array( $status['files'] ?? null ) ? array_values( array_filter( $status['files'], 'is_string' ) ) : array();
        if ( (int) ( $status['dirty'] ?? 0 ) <= 0 && empty( $files ) ) {
            return array( 'enabled' => true, 'changed' => false, 'engine_data' => $engine_data, 'status' => $status );
        }

        $diff = homeboy_datamachine_agent_execute_workspace_ability( 'datamachine/workspace-git-diff', array( 'name' => $handle ) );
        $add  = homeboy_datamachine_agent_execute_workspace_ability( 'datamachine/workspace-git-add', array( 'name' => $handle, 'paths' => empty( $files ) ? array( '.' ) : $files ) );
        if ( empty( $add['success'] ) ) {
            return array( 'enabled' => true, 'changed' => true, 'engine_data' => $engine_data, 'status' => $status, 'diff' => $diff, 'add' => $add, 'error' => (string) ( $add['error'] ?? 'Workspace git add failed.' ) );
        }

        $workspace_config = homeboy_datamachine_agent_runner_workspace_config( $config );
        $message          = isset( $workspace_config['commit_message'] ) && is_scalar( $workspace_config['commit_message'] ) ? trim( (string) $workspace_config['commit_message'] ) : '';
        if ( '' === $message ) {
            $message = 'chore: persist Data Machine agent workspace changes';
        }

        $commit = homeboy_datamachine_agent_execute_workspace_ability( 'datamachine/workspace-git-commit', array( 'name' => $handle, 'message' => $message ) );
        if ( empty( $commit['success'] ) ) {
            return array( 'enabled' => true, 'changed' => true, 'engine_data' => $engine_data, 'status' => $status, 'diff' => $diff, 'add' => $add, 'commit' => $commit, 'error' => (string) ( $commit['error'] ?? 'Workspace git commit failed.' ) );
        }

        $push = homeboy_datamachine_agent_execute_workspace_ability( 'datamachine/workspace-git-push', array( 'name' => $handle, 'branch' => (string) ( $runner_workspace['branch'] ?? '' ) ) );
        if ( empty( $push['success'] ) ) {
            return array( 'enabled' => true, 'changed' => true, 'engine_data' => $engine_data, 'status' => $status, 'diff' => $diff, 'add' => $add, 'commit' => $commit, 'push' => $push, 'error' => (string) ( $push['error'] ?? 'Workspace git push failed.' ) );
        }

        return array(
            'enabled'               => true,
            'changed'               => true,
            'engine_data'           => $engine_data,
            'status'                => $status,
            'diff'                  => $diff,
            'add'                   => $add,
            'commit'                => $commit,
            'push'                  => $push,
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_runner_workspace_written_paths' ) ) {
    function homeboy_datamachine_agent_runner_workspace_written_paths( array $runner_workspace_capture ): array {
        $status = is_array( $runner_workspace_capture['status'] ?? null ) ? $runner_workspace_capture['status'] : array();
        return is_array( $status['files'] ?? null ) ? array_values( array_filter( $status['files'], 'is_string' ) ) : array();
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_ability_schema' ) ) {
    function homeboy_datamachine_agent_ability_schema( string $ability_name ): array {
        $ability = function_exists( 'wp_get_ability' ) ? wp_get_ability( $ability_name ) : null;
        if ( $ability && method_exists( $ability, 'get_input_schema' ) ) {
            $schema = (array) $ability->get_input_schema();
            if ( ! empty( $schema['properties'] ) && is_array( $schema['properties'] ) ) {
                return $schema;
            }
        }

        return array(
            'type'       => 'object',
            'properties' => array(),
        );
    }
}

if ( ! class_exists( 'Homeboy_Datamachine_Agent_Tool_Recorder' ) ) {
    class Homeboy_Datamachine_Agent_Tool_Recorder {
        private static array $tool_results = array();

        public static function tool_results(): array {
            return self::$tool_results;
        }

        public function handle_tool_call( array $parameters, array $tool_def = array() ): array {
            $parameters = $this->apply_forced_parameters( $parameters, $tool_def );

            $response = isset( $tool_def['homeboy_original_tool'] ) && is_array( $tool_def['homeboy_original_tool'] )
                ? $this->call_original_tool( $parameters, $tool_def['homeboy_original_tool'] )
                : $this->call_ability_tool( $parameters, $tool_def );

            $this->record( $parameters, $tool_def, $response );
            return $response;
        }

        private function apply_forced_parameters( array $parameters, array $tool_def ): array {
            $forced_parameters = is_array( $tool_def['homeboy_forced_parameters'] ?? null ) ? $tool_def['homeboy_forced_parameters'] : array();
            if ( empty( $forced_parameters ) ) {
                return $parameters;
            }

            return array_replace_recursive( $parameters, $forced_parameters );
        }

        private function call_original_tool( array $parameters, array $original_tool ): array {
            $class  = (string) ( $original_tool['class'] ?? '' );
            $method = (string) ( $original_tool['method'] ?? 'handle_tool_call' );
            if ( '' === $class || ! class_exists( $class ) || ! method_exists( $class, $method ) ) {
                return $this->error( $class, 'Original tool callback is unavailable.' );
            }

            $tool = new $class();
            $result = $tool->{$method}( $parameters, $original_tool );
            return is_array( $result ) ? $result : array( 'success' => true, 'data' => $result );
        }

        private function call_ability_tool( array $parameters, array $tool_def ): array {
            $ability_name = (string) ( $tool_def['ability'] ?? '' );
            $tool_name    = (string) ( $tool_def['tool_name'] ?? $ability_name );
            if ( '' === $ability_name || ! function_exists( 'wp_get_ability' ) ) {
                return $this->error( $tool_name, 'Missing ability contract.' );
            }

            $ability = wp_get_ability( $ability_name );
            if ( ! $ability ) {
                return $this->error( $tool_name, $ability_name . ' is not registered.' );
            }

            $result = $ability->execute( $parameters );
            if ( function_exists( 'is_wp_error' ) && is_wp_error( $result ) ) {
                return $this->error( $tool_name, $result->get_error_message() );
            }

            $response              = is_array( $result ) ? $result : array( 'success' => true, 'data' => $result );
            $response['tool_name'] = $tool_name;
            return $response;
        }

        private function record( array $parameters, array $tool_def, array $response ): void {
            $record = is_array( $tool_def['homeboy_record'] ?? null ) ? $tool_def['homeboy_record'] : array();
            $engine_key = (string) ( $record['engine_key'] ?? '' );
            $job_id     = (int) ( $parameters['job_id'] ?? 0 );

            $sources = array(
                'parameters' => $parameters,
                'response'   => $response,
                'data'       => is_array( $response['data'] ?? null ) ? $response['data'] : array(),
            );

            $payload = array();
            if ( ! empty( $record['tool_results_key'] ) ) {
                self::$tool_results[] = array(
                    'tool_name' => (string) ( $tool_def['tool_name'] ?? '' ),
                    'success'   => ! empty( $response['success'] ),
                    'repo'      => (string) ( $parameters['repo'] ?? '' ),
                    'head'      => (string) ( $parameters['head'] ?? '' ),
                    'base'      => (string) ( $parameters['base'] ?? '' ),
                    'url'       => homeboy_datamachine_agent_first_url( $response ),
                    'error'     => (string) ( $response['error'] ?? '' ),
                    'message'   => (string) ( $response['message'] ?? '' ),
                );
                $payload[ (string) $record['tool_results_key'] ] = self::$tool_results;
            }

            if ( '' === $engine_key || $job_id <= 0 || ! function_exists( 'datamachine_merge_engine_data' ) ) {
                return;
            }

            if ( is_array( $record['fields'] ?? null ) ) {
                foreach ( $record['fields'] as $field => $field_config ) {
                    if ( ! is_string( $field ) || '' === $field ) {
                        continue;
                    }
                    $value = $this->resolve_field_value( $sources, $field_config );
                    if ( null !== $value ) {
                        $payload[ $field ] = $value;
                    }
                }
            }

            if ( is_array( $record['event'] ?? null ) && $this->event_matches( $parameters, $response, $record['event'] ) ) {
                $event_url = homeboy_datamachine_agent_first_url( $response );
                if ( '' !== $event_url ) {
                    $payload[ (string) ( $record['event']['key'] ?? 'event' ) ] = array(
                        'type'   => (string) ( $record['event']['type'] ?? '' ),
                        'url'    => $event_url,
                        'repo'   => (string) ( $parameters['repo'] ?? '' ),
                        'number' => (int) ( $response['pull_number'] ?? $response['issue_number'] ?? $response['number'] ?? $parameters['pull_number'] ?? 0 ),
                    );
                }
            }

            if ( ! empty( $payload ) ) {
                datamachine_merge_engine_data( $job_id, array( $engine_key => $payload ) );
            }
        }

        private function resolve_field_value( array $sources, $field_config ) {
            $paths = is_array( $field_config ) && isset( $field_config['paths'] ) && is_array( $field_config['paths'] )
                ? $field_config['paths']
                : ( is_array( $field_config ) ? $field_config : array( $field_config ) );
            foreach ( $paths as $path ) {
                if ( ! is_scalar( $path ) ) {
                    continue;
                }
                $value = homeboy_datamachine_agent_path_value( $sources, (string) $path );
                if ( null === $value || '' === $value ) {
                    continue;
                }
                if ( is_array( $field_config ) && isset( $field_config['strip_prefix'] ) && is_string( $value ) && is_scalar( $field_config['strip_prefix'] ) ) {
                    $prefix = (string) $field_config['strip_prefix'];
                    if ( str_starts_with( $value, $prefix ) ) {
                        $value = substr( $value, strlen( $prefix ) );
                    }
                }
                return $value;
            }
            return null;
        }

        private function event_matches( array $parameters, array $response, array $event ): bool {
            if ( ! empty( $event['only_if_success'] ) && empty( $response['success'] ) ) {
                return false;
            }
            $match = is_array( $event['match'] ?? null ) ? $event['match'] : array();
            foreach ( $match as $parameter => $env_name ) {
                if ( ! is_string( $parameter ) || ! is_scalar( $env_name ) ) {
                    continue;
                }
                $expected = trim( (string) getenv( (string) $env_name ) );
                if ( '' !== $expected && (string) ( $parameters[ $parameter ] ?? '' ) !== $expected ) {
                    return false;
                }
            }
            return true;
        }

        private function error( string $tool_name, string $message ): array {
            return array(
                'success'   => false,
                'error'     => $message,
                'tool_name' => $tool_name,
            );
        }
    }
}

if ( ! class_exists( 'Homeboy_Datamachine_Agent_Terminal_Tool' ) ) {
    class Homeboy_Datamachine_Agent_Terminal_Tool {
        private function normalize_wp_cli_command( string $command ): string {
            $command = trim( $command );
            return str_starts_with( $command, 'wp ' ) ? $command : 'wp ' . $command;
        }

        private function run_runtime_wp_cli_command( string $command ): array {
            $normalized_command = $this->normalize_wp_cli_command( $command );
            $wp_cli_command     = trim( preg_replace( '/^wp\s+/', '', $normalized_command ) );

            ob_start();
            $started = microtime( true );
            $result  = WP_CLI::runcommand(
                $wp_cli_command,
                array(
                    'return'     => true,
                    'parse'      => 'shell',
                    'launch'     => false,
                    'exit_error' => false,
                )
            );
            $stdout  = (string) ob_get_clean();
            $success = false !== $result && ! is_wp_error( $result );
            $stderr  = '';

            if ( is_wp_error( $result ) ) {
                $stderr = $result->get_error_message();
            } elseif ( is_string( $result ) && '' !== $result ) {
                $stdout .= $result;
                if ( ! str_ends_with( $stdout, "\n" ) ) {
                    $stdout .= "\n";
                }
            }

            return array(
                'type'       => 'wp_cli',
                'command'    => $normalized_command,
                'exitCode'   => $success ? 0 : 1,
                'stdout'     => $stdout,
                'stderr'     => $stderr,
                'success'    => $success,
                'timedOut'   => false,
                'durationMs' => (int) round( ( microtime( true ) - $started ) * 1000 ),
                'error'      => $success ? '' : ( '' !== $stderr ? $stderr : 'WP-CLI command failed' ),
            );
        }

        private function runtime_wp_cli_eval_code( string $command ): ?string {
            $wp_cli_command = trim( preg_replace( '/^wp\s+/', '', $this->normalize_wp_cli_command( $command ) ) );
            if ( ! str_starts_with( $wp_cli_command, 'eval ' ) ) {
                return null;
            }

            $code = trim( substr( $wp_cli_command, 5 ) );
            if ( strlen( $code ) >= 2 ) {
                $quote = $code[0];
                if ( ( "'" === $quote || '"' === $quote ) && str_ends_with( $code, $quote ) ) {
                    $code = substr( $code, 1, -1 );
                }
            }

            return $code;
        }

        private function run_runtime_wp_cli_eval_command( string $command ): array {
            $normalized_command = $this->normalize_wp_cli_command( $command );
            $code               = $this->runtime_wp_cli_eval_code( $command );
            $started            = microtime( true );
            $success            = true;
            $stderr             = '';

            ob_start();
            try {
                eval( (string) $code );
            } catch ( Throwable $throwable ) {
                $success = false;
                $stderr  = $throwable->getMessage();
            }
            $stdout = (string) ob_get_clean();

            return array(
                'type'       => 'wp_cli',
                'command'    => $normalized_command,
                'exitCode'   => $success ? 0 : 1,
                'stdout'     => $stdout,
                'stderr'     => $stderr,
                'success'    => $success,
                'timedOut'   => false,
                'durationMs' => (int) round( ( microtime( true ) - $started ) * 1000 ),
                'error'      => $success ? '' : ( '' !== $stderr ? $stderr : 'WP-CLI eval command failed' ),
            );
        }

        public function handle_tool_call( array $parameters, array $tool_def = array() ): array {
            $type = (string) ( $tool_def['terminal_action_type'] ?? 'wp_cli' );

            $command = trim( (string) ( $parameters['command'] ?? '' ) );
            if ( '' === $command ) {
                return array(
                    'success'   => false,
                    'tool_name' => (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' ),
                    'error'     => 'command is required.',
                );
            }

            if ( 'wp_cli' === $type && class_exists( 'WP_CLI' ) && method_exists( 'WP_CLI', 'runcommand' ) ) {
                $result              = $this->run_runtime_wp_cli_command( $command );
                $result['tool_name'] = (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' );
                $result['status']    = 200;
                return $result;
            }

            if ( 'wp_cli' === $type && null !== $this->runtime_wp_cli_eval_code( $command ) ) {
                $result              = $this->run_runtime_wp_cli_eval_command( $command );
                $result['tool_name'] = (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' );
                $result['status']    = 200;
                return $result;
            }

            $url   = rtrim( (string) ( $tool_def['terminal_action_url'] ?? getenv( 'HOMEBOY_TERMINAL_ACTION_URL' ) ), '/' );
            $token = (string) ( $tool_def['terminal_action_token'] ?? getenv( 'HOMEBOY_TERMINAL_ACTION_TOKEN' ) );

            if ( '' === $url || '' === $token ) {
                return array(
                    'success'   => false,
                    'tool_name' => (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' ),
                    'error'     => 'Terminal action server is not configured.',
                );
            }

            $action = array_filter(
                array(
                    'type'       => $type,
                    'command'    => $command,
                    'timeout_ms' => isset( $parameters['timeout_ms'] ) ? (int) $parameters['timeout_ms'] : null,
                    'cwd'        => isset( $parameters['cwd'] ) ? (string) $parameters['cwd'] : null,
                ),
                static fn( $value ) => null !== $value && '' !== $value
            );

            $response = wp_remote_post(
                $url . '/execute',
                array(
                    'timeout' => max( 1, (int) ceil( ( (int) ( $action['timeout_ms'] ?? 30000 ) + 5000 ) / 1000 ) ),
                    'headers' => array(
                        'Authorization' => 'Bearer ' . $token,
                        'Content-Type'  => 'application/json',
                    ),
                    'body'    => wp_json_encode( $action ),
                )
            );

            if ( is_wp_error( $response ) ) {
                return array(
                    'success'   => false,
                    'tool_name' => (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' ),
                    'error'     => $response->get_error_message(),
                );
            }

            $status = (int) wp_remote_retrieve_response_code( $response );
            $body   = (string) wp_remote_retrieve_body( $response );
            $result = json_decode( $body, true );
            if ( ! is_array( $result ) ) {
                return array(
                    'success'   => false,
                    'tool_name' => (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' ),
                    'error'     => 'Terminal action server returned invalid JSON.',
                    'status'    => $status,
                    'body'      => $body,
                );
            }

            $result['tool_name'] = (string) ( $tool_def['tool_name'] ?? 'run_wp_cli' );
            $result['status']    = $status;
            return $result;
        }
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_register_tool_recorders' ) ) {
    function homeboy_datamachine_agent_tool_explicitly_configured( array $config, string $tool_name ): bool {
        foreach ( array( 'ability_tools', 'tool_recorders' ) as $config_key ) {
            $tool_configs = is_array( $config[ $config_key ] ?? null ) ? $config[ $config_key ] : array();
            foreach ( $tool_configs as $tool_config ) {
                if ( is_array( $tool_config ) && $tool_name === (string) ( $tool_config['name'] ?? $tool_config['tool'] ?? '' ) ) {
                    return true;
                }
            }
        }

        return false;
    }

    function homeboy_datamachine_agent_register_tool_recorders( array $config ): void {
        $ability_tools = is_array( $config['ability_tools'] ?? null ) ? $config['ability_tools'] : array();
        $recorders     = is_array( $config['tool_recorders'] ?? null ) ? $config['tool_recorders'] : array();
        $allow_pr_tool = homeboy_datamachine_agent_tool_explicitly_configured( $config, 'create_github_pull_request' );

        $recorded_tools = array();
        foreach ( $recorders as $recorder ) {
            if ( is_array( $recorder ) && ! empty( $recorder['tool'] ) && is_scalar( $recorder['tool'] ) ) {
                $recorded_tools[] = (string) $recorder['tool'];
            }
        }

        $tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
        foreach ( array( 'create_or_update_github_file' ) as $tool_name ) {
            if ( in_array( $tool_name, $recorded_tools, true ) ) {
                continue;
            }

            $recorders[] = array(
                'tool'   => $tool_name,
                'record' => array(
                    'tool_results_key' => $tool_results_key,
                ),
            );
        }
        if ( empty( $ability_tools ) && empty( $recorders ) ) {
            return;
        }

        add_filter(
            'datamachine_resolved_tools',
            static function ( array $tools ) use ( $ability_tools, $recorders, $allow_pr_tool ): array {
                if ( ! $allow_pr_tool ) {
                    unset( $tools['create_github_pull_request'] );
                }

                foreach ( $ability_tools as $tool_config ) {
                    if ( ! is_array( $tool_config ) ) {
                        continue;
                    }
                    $tool_name    = (string) ( $tool_config['name'] ?? '' );
                    $ability_name = (string) ( $tool_config['ability'] ?? '' );
                    if ( '' === $tool_name || '' === $ability_name ) {
                        continue;
                    }
                    $tools[ $tool_name ] = array(
                        'class'                      => 'Homeboy_Datamachine_Agent_Tool_Recorder',
                        'method'                     => 'handle_tool_call',
                        'ability'                    => $ability_name,
                        'tool_name'                  => $tool_name,
                        'description'                => (string) ( $tool_config['description'] ?? 'Execute ' . $ability_name . '.' ),
                        'parameters'                 => homeboy_datamachine_agent_ability_schema( $ability_name ),
                        'homeboy_record'             => is_array( $tool_config['record'] ?? null ) ? $tool_config['record'] : array(),
                        'homeboy_forced_parameters' => is_array( $tool_config['forced_parameters'] ?? null ) ? $tool_config['forced_parameters'] : array(),
                    );
                }

                foreach ( $recorders as $recorder ) {
                    if ( ! is_array( $recorder ) ) {
                        continue;
                    }
                    $tool_name = (string) ( $recorder['tool'] ?? '' );
                    if ( '' === $tool_name || empty( $tools[ $tool_name ] ) || ! is_array( $tools[ $tool_name ] ) ) {
                        continue;
                    }
                    $original_tool = $tools[ $tool_name ];
                    $tools[ $tool_name ]['class']                      = 'Homeboy_Datamachine_Agent_Tool_Recorder';
                    $tools[ $tool_name ]['method']                     = 'handle_tool_call';
                    $tools[ $tool_name ]['tool_name']                  = $tool_name;
                    $tools[ $tool_name ]['homeboy_original_tool']      = $original_tool;
                    $tools[ $tool_name ]['homeboy_record']             = is_array( $recorder['record'] ?? null ) ? $recorder['record'] : array();
                    $tools[ $tool_name ]['homeboy_forced_parameters'] = is_array( $recorder['forced_parameters'] ?? null ) ? $recorder['forced_parameters'] : array();
                }

                return $tools;
            },
            100,
            1
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_register_terminal_tools' ) ) {
    function homeboy_datamachine_agent_register_terminal_tools( array $config ): void {
        if ( empty( $config['enable_terminal_actions'] ) && empty( $config['enable_wp_cli_tool'] ) ) {
            return;
        }

        $terminal_url   = homeboy_datamachine_agent_scalar( $config, 'terminal_action_url' );
        $terminal_token = homeboy_datamachine_agent_scalar( $config, 'terminal_action_token' );
        $tool_name      = homeboy_datamachine_agent_scalar( $config, 'wp_cli_tool_name', 'run_wp_cli' );

        add_filter(
            'datamachine_resolved_tools',
            static function ( array $tools ) use ( $terminal_url, $terminal_token, $tool_name ): array {
                $tools[ $tool_name ] = array(
                    'class'                 => 'Homeboy_Datamachine_Agent_Terminal_Tool',
                    'method'                => 'handle_tool_call',
                    'tool_name'             => $tool_name,
                    'description'           => 'Run a real WP-CLI command through the host terminal against the disposable WordPress runtime. Returns exit code, stdout, and stderr.',
                    'terminal_action_type'  => 'wp_cli',
                    'terminal_action_url'   => $terminal_url,
                    'terminal_action_token' => $terminal_token,
                    'parameters'            => array(
                        'command'    => array(
                            'type'        => 'string',
                            'required'    => true,
                            'description' => 'WP-CLI command to run. You may include or omit the leading `wp`.',
                        ),
                        'timeout_ms' => array(
                            'type'        => 'integer',
                            'description' => 'Maximum command runtime in milliseconds.',
                        ),
                    ),
                );

                return $tools;
            },
            100,
            1
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_register_directive_controls' ) ) {
    function homeboy_datamachine_agent_register_directive_controls( array $config ): void {
        if ( empty( $config['disable_datamachine_directives'] ) ) {
            return;
        }

        add_filter( 'datamachine_directives_enabled', '__return_false', 100, 3 );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_array_merge_recursive_distinct' ) ) {
    function homeboy_datamachine_agent_array_merge_recursive_distinct( array $base, array $patch ): array {
        foreach ( $patch as $key => $value ) {
            if ( is_array( $value ) && isset( $base[ $key ] ) && is_array( $base[ $key ] ) ) {
                $base[ $key ] = homeboy_datamachine_agent_array_merge_recursive_distinct( $base[ $key ], $value );
                continue;
            }
            $base[ $key ] = $value;
        }
        return $base;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_apply_step_patches' ) ) {
    function homeboy_datamachine_agent_apply_step_patches( array $steps, array $patches ): array {
        if ( empty( $patches ) ) {
            return $steps;
        }

        foreach ( $steps as &$step_config ) {
            if ( ! is_array( $step_config ) ) {
                continue;
            }
            foreach ( $patches as $patch ) {
                if ( ! is_array( $patch ) ) {
                    continue;
                }
                $step_type = (string) ( $patch['step_type'] ?? '' );
                $step_id   = (string) ( $patch['step_id'] ?? '' );
                if ( '' !== $step_type && $step_type !== (string) ( $step_config['step_type'] ?? '' ) ) {
                    continue;
                }
                if ( '' !== $step_id && $step_id !== (string) ( $step_config['pipeline_step_id'] ?? $step_config['flow_step_id'] ?? '' ) ) {
                    continue;
                }
                if ( is_array( $patch['set'] ?? null ) ) {
                    foreach ( $patch['set'] as $key => $value ) {
                        if ( is_string( $key ) && '' !== $key ) {
                            $step_config[ $key ] = $value;
                        }
                    }
                }
                if ( is_array( $patch['merge'] ?? null ) ) {
                    $step_config = homeboy_datamachine_agent_array_merge_recursive_distinct( $step_config, $patch['merge'] );
                }
            }
        }
        unset( $step_config );

        return $steps;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_bool_config' ) ) {
    function homeboy_datamachine_agent_bool_config( array $config, string $key, bool $default = false ): bool {
        if ( ! array_key_exists( $key, $config ) ) {
            return $default;
        }

        return filter_var( $config[ $key ], FILTER_VALIDATE_BOOLEAN );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_slug' ) ) {
    function homeboy_datamachine_agent_slug( string $value ): string {
        $slug = strtolower( preg_replace( '/[^a-zA-Z0-9]+/', '-', $value ) ?? '' );
        return trim( $slug, '-' );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_provision_workspace' ) ) {
    function homeboy_datamachine_agent_provision_workspace( array $config ): array {
        $workspace = homeboy_datamachine_agent_runner_workspace_config( $config );
        if ( ! homeboy_datamachine_agent_bool_config( $workspace, 'enabled', false ) ) {
            return array( 'enabled' => false );
        }

        $repo = isset( $workspace['repo'] ) && is_scalar( $workspace['repo'] ) ? trim( (string) $workspace['repo'] ) : '';
        if ( str_contains( $repo, '/' ) ) {
            $repo = basename( $repo );
        }
        $repo = preg_replace( '/\.git$/', '', $repo ) ?? $repo;
        if ( '' === $repo ) {
            return array( 'enabled' => true, 'success' => false, 'error' => 'runner_workspace.repo is required' );
        }

        $required = array(
            'datamachine/workspace-show',
            'datamachine/workspace-clone',
            'datamachine/workspace-worktree-add',
        );
        foreach ( $required as $ability_name ) {
            if ( ! wp_get_ability( $ability_name ) ) {
                return array( 'enabled' => true, 'success' => false, 'error' => $ability_name . ' is not registered' );
            }
        }

        $show = wp_get_ability( 'datamachine/workspace-show' )->execute( array( 'name' => $repo ) );
        if ( function_exists( 'is_wp_error' ) && is_wp_error( $show ) ) {
            $clone_url = isset( $workspace['clone_url'] ) && is_scalar( $workspace['clone_url'] ) ? trim( (string) $workspace['clone_url'] ) : '';
            if ( '' === $clone_url ) {
                return array( 'enabled' => true, 'success' => false, 'error' => 'workspace primary missing and runner_workspace.clone_url is empty' );
            }

            $clone_input = array(
                'url'  => $clone_url,
                'name' => $repo,
            );
            $github_token_env = homeboy_datamachine_agent_scalar( $config, 'github_token_env', 'GITHUB_TOKEN' );
            if ( '' !== $github_token_env && '' !== trim( (string) getenv( $github_token_env ) ) && preg_match( '#^https://github\.com/#', $clone_url ) ) {
                $clone_input['auth_token_env'] = $github_token_env;
            }

            $clone = wp_get_ability( 'datamachine/workspace-clone' )->execute(
                array_filter(
                    $clone_input,
                    static fn( $value ) => '' !== $value
                )
            );
            if ( function_exists( 'is_wp_error' ) && is_wp_error( $clone ) ) {
                return array( 'enabled' => true, 'success' => false, 'error' => $clone->get_error_message() );
            }
        }

        $branch = isset( $workspace['branch'] ) && is_scalar( $workspace['branch'] ) ? trim( (string) $workspace['branch'] ) : '';
        if ( '' === $branch ) {
            $prefix = isset( $workspace['branch_prefix'] ) && is_scalar( $workspace['branch_prefix'] ) ? trim( (string) $workspace['branch_prefix'] ) : 'agent-run';
            $seed   = homeboy_datamachine_agent_slug( homeboy_datamachine_agent_scalar( $config, 'workload_id', 'datamachine-agent' ) );
            $branch = rtrim( $prefix, '/' ) . '/' . gmdate( 'Y-m-d-His' ) . ( '' !== $seed ? '-' . $seed : '' );
        }

        $input = array(
            'repo'           => $repo,
            'branch'         => $branch,
            'from'           => isset( $workspace['from'] ) && is_scalar( $workspace['from'] ) ? trim( (string) $workspace['from'] ) : 'origin/HEAD',
            'inject_context' => homeboy_datamachine_agent_bool_config( $workspace, 'inject_context', true ),
            'bootstrap'      => homeboy_datamachine_agent_bool_config( $workspace, 'bootstrap', true ),
            'allow_stale'    => homeboy_datamachine_agent_bool_config( $workspace, 'allow_stale', false ),
            'rebase_base'    => homeboy_datamachine_agent_bool_config( $workspace, 'rebase_base', false ),
            'force'          => homeboy_datamachine_agent_bool_config( $workspace, 'force', false ),
        );

        $worktree = wp_get_ability( 'datamachine/workspace-worktree-add' )->execute( $input );
        if ( function_exists( 'is_wp_error' ) && is_wp_error( $worktree ) ) {
            return array( 'enabled' => true, 'success' => false, 'error' => $worktree->get_error_message(), 'input' => $input );
        }
        if ( ! is_array( $worktree ) || empty( $worktree['success'] ) ) {
            return array( 'enabled' => true, 'success' => false, 'error' => 'workspace-worktree-add did not succeed', 'input' => $input, 'result' => $worktree );
        }

        return array(
            'enabled' => true,
            'success' => true,
            'repo'    => $repo,
            'branch'  => (string) ( $worktree['branch'] ?? $branch ),
            'handle'  => (string) ( $worktree['handle'] ?? '' ),
            'path'    => (string) ( $worktree['path'] ?? '' ),
            'input'   => $input,
            'result'  => $worktree,
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_apply_runner_workspace' ) ) {
    function homeboy_datamachine_agent_apply_runner_workspace( array $config, string $prompt, array $runner_workspace ): array {
        if ( empty( $runner_workspace['success'] ) || empty( $runner_workspace['handle'] ) ) {
            return array( $config, $prompt );
        }

        $handle       = (string) $runner_workspace['handle'];
        $agent_alias  = homeboy_datamachine_agent_runner_workspace_alias( $config );
        $agent_root   = homeboy_datamachine_agent_runner_workspace_root( $config );
        $agent_handle = '' !== $agent_alias ? $agent_alias : $handle;
        if ( '' !== $agent_alias ) {
            add_filter(
                'datamachine_code_workspace_aliases',
                static function ( array $aliases ) use ( $agent_alias, $handle, $agent_root ): array {
                    $aliases[ $agent_alias ] = '' !== $agent_root
                        ? array(
                            'target' => $handle,
                            'root'   => $agent_root,
                        )
                        : $handle;
                    return $aliases;
                }
            );
        }

        if ( homeboy_datamachine_agent_runner_workspace_exposed( $config ) ) {
            $prefix = '' !== $agent_alias
                ? "Project workspace:\nUse `{$agent_alias}` for repository file and git changes when a workspace tool asks for a project or repository name."
                : "Runner-provided workspace:\n- Workspace handle: {$handle}\n- Branch: " . (string) ( $runner_workspace['branch'] ?? '' ) . "\n\nUse this workspace handle for all repository file and git changes. Do not mutate the primary checkout and do not create another worktree for this run.";
            $prompt = '' === trim( $prompt ) ? $prefix : $prefix . "\n\n" . $prompt;
        }

        $recorders        = is_array( $config['tool_recorders'] ?? null ) ? $config['tool_recorders'] : array();
        $tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
        $record_config    = array( 'tool_results_key' => $tool_results_key );
        foreach ( array( 'workspace_ls', 'workspace_read', 'workspace_grep', 'workspace_write', 'workspace_edit', 'workspace_apply_patch', 'workspace_delete' ) as $tool_name ) {
            $recorders[] = array(
                'tool'              => $tool_name,
                'forced_parameters' => array( 'repo' => $agent_handle ),
                'record'            => $record_config,
            );
        }
        foreach ( array( 'workspace_git_status', 'workspace_git_log', 'workspace_git_diff', 'workspace_git_pull', 'workspace_git_add', 'workspace_git_commit', 'workspace_git_push' ) as $tool_name ) {
            $recorders[] = array(
                'tool'              => $tool_name,
                'forced_parameters' => array( 'name' => $agent_handle ),
                'record'            => $record_config,
            );
        }
        $config['tool_recorders'] = $recorders;

        return array( $config, $prompt );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_bootstrap_provider' ) ) {
    function homeboy_datamachine_agent_bootstrap_provider( array $config ): void {
        $function = homeboy_datamachine_agent_scalar( $config, 'provider_register_function' );
        if ( '' !== $function && function_exists( $function ) ) {
            call_user_func( $function );
        }
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_bootstrap_abilities' ) ) {
    function homeboy_datamachine_agent_bootstrap_abilities(): ?array {
        if ( ! function_exists( 'did_action' ) || ! function_exists( 'do_action' ) ) {
            return homeboy_datamachine_agent_result( array( 'has_actions_api' => 0 ), array(), 'WordPress action API not available' );
        }

        if ( ! did_action( 'wp_abilities_api_categories_init' ) ) {
            do_action( 'wp_abilities_api_categories_init' );
        }
        if ( ! did_action( 'wp_abilities_api_init' ) ) {
            do_action( 'wp_abilities_api_init' );
        }

        if ( ! function_exists( 'wp_get_ability' ) ) {
            return homeboy_datamachine_agent_result( array( 'has_abilities_api' => 0 ), array(), 'Abilities API not loaded' );
        }

        return null;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_configure_settings' ) ) {
    function homeboy_datamachine_agent_configure_settings( array $config ): array {
        $provider = homeboy_datamachine_agent_scalar( $config, 'provider', 'openai' );
        $model    = homeboy_datamachine_agent_scalar( $config, 'model', 'gpt-5.5' );
        $settings = function_exists( 'get_option' ) ? (array) get_option( 'datamachine_settings', array() ) : array();

        $github_token_env = homeboy_datamachine_agent_scalar( $config, 'github_token_env', 'GITHUB_TOKEN' );
        $github_token     = trim( (string) getenv( $github_token_env ) );
        $github_repository_token_env = homeboy_datamachine_agent_scalar( $config, 'github_repository_token_env', '' );
        $github_repository_token     = '' !== $github_repository_token_env ? trim( (string) getenv( $github_repository_token_env ) ) : '';
        $target_repo      = homeboy_datamachine_agent_scalar( $config, 'target_repo' );
        if ( '' !== $target_repo && ( '' !== $github_token || '' !== $github_repository_token ) ) {
            $allowed_repos = is_array( $config['allowed_repos'] ?? null ) ? $config['allowed_repos'] : array( $target_repo );
            $profile_id    = homeboy_datamachine_agent_scalar( $config, 'github_profile_id', 'homeboy-agent-ci' );
            $profiles      = array();
            if ( '' !== $github_token ) {
                $profiles[] = array(
                    'id'            => $profile_id,
                    'label'         => 'Homeboy agent CI token',
                    'mode'          => 'pat',
                    'pat'           => $github_token,
                    'default_repo'  => $target_repo,
                    'allowed_repos' => array_values( array_unique( array_filter( array_map( 'strval', $allowed_repos ) ) ) ),
                );
            }
            if ( '' !== $github_repository_token ) {
                $profiles[] = array(
                    'id'            => $profile_id . '-repository',
                    'label'         => 'Homeboy agent CI repository token',
                    'mode'          => 'pat',
                    'pat'           => $github_repository_token,
                    'default_repo'  => $target_repo,
                    'allowed_repos' => array( $target_repo ),
                );
            }
            $settings['github_credential_profiles'] = $profiles;
            $settings['github_default_profile_id']  = '' !== $github_token ? $profile_id : $profile_id . '-repository';
            $settings['github_default_repo']       = $target_repo;
        }

        $settings['default_provider'] = $provider;
        $settings['default_model']    = $model;
        $settings['mode_models']      = array(
            'pipeline' => array( 'provider' => $provider, 'model' => $model ),
            'chat'     => array( 'provider' => $provider, 'model' => $model ),
            'system'   => array( 'provider' => $provider, 'model' => $model ),
        );
        $settings['max_turns']        = isset( $config['max_turns'] ) ? max( 1, (int) $config['max_turns'] ) : 12;

        if ( isset( $config['daily_memory_enabled'] ) ) {
            $settings['daily_memory_enabled'] = (bool) $config['daily_memory_enabled'];
        }

        update_option( 'datamachine_settings', $settings, false );

        $credential_options = is_array( $config['provider_credentials'] ?? null ) ? $config['provider_credentials'] : array();
        foreach ( $credential_options as $option_name => $env_name ) {
            if ( ! is_string( $option_name ) || '' === $option_name || ! is_scalar( $env_name ) ) {
                continue;
            }
            $credential = trim( (string) getenv( (string) $env_name ) );
            if ( '' !== $credential ) {
                update_option( $option_name, $credential, false );
            }
        }

        if ( class_exists( PluginSettings::class ) ) {
            PluginSettings::clearCache();
        }

        return $settings;
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_export_transcript' ) ) {
    function homeboy_datamachine_agent_export_transcript( int $job_id, array $engine_data, string $transcript_dir ): array {
        $session_id = (string) ( $engine_data['transcript_session_id'] ?? '' );
        if ( '' === $session_id || '' === $transcript_dir || ! class_exists( ConversationStoreFactory::class ) ) {
            return array();
        }

        $store   = ConversationStoreFactory::get();
        $session = $store->get_session( $session_id );
        if ( ! $session ) {
            return array( 'session_id' => $session_id, 'error' => 'Transcript session missing' );
        }

        if ( ! is_dir( $transcript_dir ) && function_exists( 'wp_mkdir_p' ) && ! wp_mkdir_p( $transcript_dir ) ) {
            return array( 'session_id' => $session_id, 'error' => 'Transcript directory could not be created' );
        }

        $path    = rtrim( $transcript_dir, '/' ) . '/job-' . $job_id . '-transcript.json';
        $content = wp_json_encode(
            array(
                'job_id'     => $job_id,
                'session_id' => $session_id,
                'provider'   => $session['provider'] ?? null,
                'model'      => $session['model'] ?? null,
                'metadata'   => is_array( $session['metadata'] ?? null ) ? $session['metadata'] : array(),
                'messages'   => is_array( $session['messages'] ?? null ) ? $session['messages'] : array(),
            ),
            JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE
        );

        file_put_contents( $path, $content );

        return array(
            'session_id' => $session_id,
            'json'       => $path,
            'content'    => $content,
        );
    }
}

if ( ! function_exists( 'homeboy_datamachine_agent_drain_job' ) ) {
    function homeboy_datamachine_agent_drain_job( int $job_id, array $config, Jobs $jobs ): array {
        $started_at              = hrtime( true );
        $retry_wait_budget_ms    = isset( $config['retry_wait_budget_ms'] ) ? max( 0, (int) $config['retry_wait_budget_ms'] ) : 180000;
        $retry_max_sleep_ms      = isset( $config['retry_max_sleep_ms'] ) ? max( 1000, (int) $config['retry_max_sleep_ms'] ) : 30000;
        $step_budget             = isset( $config['step_budget'] ) ? max( 1, (int) $config['step_budget'] ) : 20;
        $time_budget_ms          = isset( $config['time_budget_ms'] ) ? max( 1000, (int) $config['time_budget_ms'] ) : 300000;
        $history                 = array();
        $drain_result            = array( 'success' => false );
        $waited_ms               = 0;

        do {
            $drain_result = wp_get_ability( 'datamachine/drain-job' )->execute(
                array(
                    'job_id'         => $job_id,
                    'step_budget'    => $step_budget,
                    'time_budget_ms' => $time_budget_ms,
                )
            );

            $job         = $jobs->get_job( $job_id );
            $job_status  = is_array( $job ) ? (string) ( $job['status'] ?? '' ) : '';
            $engine_data = function_exists( 'datamachine_get_engine_data' ) ? datamachine_get_engine_data( $job_id ) : array();

            $history[] = array(
                'drain_result' => $drain_result,
                'job_status'   => $job_status,
                'retry'        => is_array( $engine_data['retry'] ?? null ) ? $engine_data['retry'] : array(),
            );

            if ( is_array( $drain_result ) && ! empty( $drain_result['success'] ) ) {
                break;
            }

            if ( in_array( $job_status, array( 'completed', 'failed', 'cancelled' ), true ) ) {
                break;
            }

            $retry = is_array( $engine_data['retry'] ?? null ) ? $engine_data['retry'] : array();
            if ( empty( $retry['last_retryable'] ) || empty( $retry['next_retry_at'] ) ) {
                break;
            }

            $next_retry_ts = strtotime( (string) $retry['next_retry_at'] );
            if ( false === $next_retry_ts ) {
                break;
            }

            $remaining_budget_ms = $retry_wait_budget_ms - $waited_ms;
            if ( $remaining_budget_ms <= 0 ) {
                break;
            }

            $delay_ms = max( 0, ( $next_retry_ts - time() ) * 1000 ) + 1000;
            $sleep_ms = min( $delay_ms, $retry_max_sleep_ms, $remaining_budget_ms );
            if ( $sleep_ms > 0 ) {
                usleep( $sleep_ms * 1000 );
                $waited_ms += $sleep_ms;
            }
        } while ( $waited_ms <= $retry_wait_budget_ms );

        return array(
            'drain_result'     => $drain_result,
            'drain_elapsed_ms' => ( hrtime( true ) - $started_at ) / 1000000,
            'drain_history'    => $history,
            'retry_waited_ms'  => $waited_ms,
        );
    }

    function homeboy_datamachine_agent_drain_child_jobs( int $parent_job_id, array $config, Jobs $jobs ): array {
        if ( ! method_exists( $jobs, 'get_children' ) ) {
            return array(
                'children'      => array(),
                'drain_results' => array(),
            );
        }

        $children = $jobs->get_children( $parent_job_id );
        $drain_results = array();
        foreach ( $children as $child_job ) {
            $child_job_id = (int) ( $child_job['job_id'] ?? 0 );
            if ( $child_job_id <= 0 ) {
                continue;
            }

            $drain_results[] = array(
                'job_id'  => $child_job_id,
                'summary' => homeboy_datamachine_agent_drain_job( $child_job_id, $config, $jobs ),
            );
        }

        return array(
            'children'      => $jobs->get_children( $parent_job_id ),
            'drain_results' => $drain_results,
        );
    }
}

if ( function_exists( 'wp_set_current_user' ) ) {
    wp_set_current_user( 1 );
}

$config = homeboy_datamachine_agent_config();
if ( empty( $config ) ) {
    return homeboy_datamachine_agent_result( array( 'config_present' => 0 ), array(), 'HOMEBOY_DATAMACHINE_AGENT_CONFIG is required' );
}

if ( ! empty( $config['dry_run'] ) ) {
    return homeboy_datamachine_agent_result(
        array( 'config_present' => 1, 'dry_run' => 1 ),
		array(
			'bundle_path'         => homeboy_datamachine_agent_scalar( $config, 'bundle_path' ),
			'bundle_repo'         => homeboy_datamachine_agent_scalar( $config, 'bundle_repo' ),
			'bundle_ref'          => homeboy_datamachine_agent_scalar( $config, 'bundle_ref' ),
			'bundle_path_in_repo' => homeboy_datamachine_agent_scalar( $config, 'bundle_path_in_repo' ),
			'task_id'             => homeboy_datamachine_agent_scalar( $config, 'task_id', homeboy_datamachine_agent_scalar( $config, 'workload_id' ) ),
			'task_label'          => homeboy_datamachine_agent_scalar( $config, 'task_label', homeboy_datamachine_agent_scalar( $config, 'workload_label' ) ),
			'agent_slug'          => homeboy_datamachine_agent_scalar( $config, 'agent_slug' ),
			'flow_slug'           => homeboy_datamachine_agent_scalar( $config, 'flow_slug' ),
			'provider'            => homeboy_datamachine_agent_scalar( $config, 'provider', 'openai' ),
			'model'               => homeboy_datamachine_agent_scalar( $config, 'model', 'gpt-5.5' ),
			'prompt'              => homeboy_datamachine_agent_scalar( $config, 'prompt' ),
			'tool_audit_events'   => is_array( $config['tool_audit_events'] ?? null ) ? $config['tool_audit_events'] : array(),
			'datamachine_provenance' => is_array( $config['datamachine_provenance'] ?? null ) ? $config['datamachine_provenance'] : array(),
			'datamachine_code_policy_attestation' => is_array( $config['datamachine_code_policy_attestation'] ?? null ) ? $config['datamachine_code_policy_attestation'] : array(),
			'fingerprints'        => homeboy_datamachine_agent_fingerprints( $config, homeboy_datamachine_agent_scalar( $config, 'prompt' ), homeboy_datamachine_agent_scalar( $config, 'bundle_path' ) ),
			'runtime_versions'    => homeboy_datamachine_agent_runtime_versions(),
		)
	);
}

$bundle_path = homeboy_datamachine_agent_scalar( $config, 'bundle_path' );
$agent_slug  = homeboy_datamachine_agent_scalar( $config, 'agent_slug' );
$flow_slug   = homeboy_datamachine_agent_scalar( $config, 'flow_slug' );
$prompt      = homeboy_datamachine_agent_scalar( $config, 'prompt' );
$prompt_env  = homeboy_datamachine_agent_scalar( $config, 'prompt_env' );
if ( '' === $prompt && '' !== $prompt_env ) {
    $prompt = trim( (string) getenv( $prompt_env ) );
}

$metadata = array(
    'bundle_path'   => $bundle_path,
    'agent_slug'    => $agent_slug,
    'flow_slug'     => $flow_slug,
    'target_repo'   => homeboy_datamachine_agent_scalar( $config, 'target_repo' ),
    'provider'      => homeboy_datamachine_agent_scalar( $config, 'provider', 'openai' ),
	'model'         => homeboy_datamachine_agent_scalar( $config, 'model', 'gpt-5.5' ),
	'task_id'      => homeboy_datamachine_agent_scalar( $config, 'task_id', homeboy_datamachine_agent_scalar( $config, 'workload_id' ) ),
	'task_label'   => homeboy_datamachine_agent_scalar( $config, 'task_label', homeboy_datamachine_agent_scalar( $config, 'workload_label' ) ),
	'prompt'        => $prompt,
	'fingerprints'  => homeboy_datamachine_agent_fingerprints( $config, $prompt, $bundle_path ),
	'runtime_versions' => homeboy_datamachine_agent_runtime_versions(),
	'bundle_exists' => '' !== $bundle_path && is_dir( $bundle_path ),
	'rules'         => is_array( $config['rules'] ?? null ) ? $config['rules'] : array(),
	'general_rules' => is_array( $config['general_rules'] ?? null ) ? $config['general_rules'] : array(),
	'task_rules'    => is_array( $config['task_rules'] ?? null ) ? $config['task_rules'] : array(),
	'probes'        => is_array( $config['probes'] ?? null ) ? $config['probes'] : array(),
	'datamachine_provenance' => is_array( $config['datamachine_provenance'] ?? null ) ? $config['datamachine_provenance'] : array(),
	'datamachine_code_policy_attestation' => is_array( $config['datamachine_code_policy_attestation'] ?? null ) ? $config['datamachine_code_policy_attestation'] : array(),
);
$execute_workflow_path = homeboy_datamachine_agent_scalar( $config, 'execute_workflow_path' );

foreach ( array( 'bundle_path' => $bundle_path, 'agent_slug' => $agent_slug, 'flow_slug' => $flow_slug ) as $label => $value ) {
    if ( '' === $value ) {
        return homeboy_datamachine_agent_result( array( $label . '_present' => 0 ), $metadata, $label . ' is required' );
    }
}
if ( ! $metadata['bundle_exists'] || ! is_file( $bundle_path . '/manifest.json' ) ) {
    return homeboy_datamachine_agent_result( array( 'bundle_exists' => 0 ), $metadata, 'Agent bundle directory missing or incomplete' );
}

homeboy_datamachine_agent_bootstrap_provider( $config );
$bootstrap_error = homeboy_datamachine_agent_bootstrap_abilities();
if ( null !== $bootstrap_error ) {
    return $bootstrap_error;
}

$runner_workspace = homeboy_datamachine_agent_provision_workspace( $config );
if ( ! empty( $runner_workspace['enabled'] ) ) {
    $metadata['runner_workspace'] = $runner_workspace;
    if ( empty( $runner_workspace['success'] ) ) {
        return homeboy_datamachine_agent_result( array( 'runner_workspace_provisioned' => 0 ), $metadata, (string) ( $runner_workspace['error'] ?? 'Runner workspace provisioning failed' ) );
    }
    $config['runner_workspace_result'] = $runner_workspace;
    list( $config, $prompt ) = homeboy_datamachine_agent_apply_runner_workspace( $config, $prompt, $runner_workspace );
}

$required_abilities = is_array( $config['required_abilities'] ?? null )
    ? $config['required_abilities']
    : ( '' !== $execute_workflow_path ? array( 'datamachine/import-agent', 'datamachine/execute-workflow', 'datamachine/drain-job' ) : array( 'datamachine/import-agent', 'datamachine/run-flow', 'datamachine/drain-job' ) );
foreach ( $required_abilities as $ability_name ) {
    if ( ! is_string( $ability_name ) || ! wp_get_ability( $ability_name ) ) {
        return homeboy_datamachine_agent_result( array( 'required_abilities_resolved' => 0 ), $metadata, (string) $ability_name . ' not registered' );
    }
}

foreach ( array( Agents::class, Pipelines::class, Flows::class, Jobs::class ) as $class_name ) {
    if ( ! class_exists( $class_name ) ) {
        return homeboy_datamachine_agent_result( array( 'required_classes_available' => 0 ), $metadata, $class_name . ' is not available' );
    }
}

$settings = homeboy_datamachine_agent_configure_settings( $config );
homeboy_datamachine_agent_register_directive_controls( $config );
homeboy_datamachine_agent_register_terminal_tools( $config );
homeboy_datamachine_agent_register_tool_recorders( $config );

$agents = new Agents();
$pipelines = new Pipelines();
$flows = new Flows();
$jobs = new Jobs();
$agent_id = 0;
$pipeline_id = 0;
$flow_id = 0;
$pipeline_slug = homeboy_datamachine_agent_scalar( $config, 'pipeline_slug' );
$import_elapsed_ms = 0;

if ( '' !== $execute_workflow_path ) {
	$import_start = hrtime( true );
	$import_result = wp_get_ability( 'datamachine/import-agent' )->execute(
		array(
			'source'      => $bundle_path,
			'on_conflict' => homeboy_datamachine_agent_scalar( $config, 'on_conflict', 'skip' ),
		)
	);
	$import_elapsed_ms = ( hrtime( true ) - $import_start ) / 1000000;
	$metadata['import_result'] = $import_result;
	if ( ! is_array( $import_result ) || empty( $import_result['success'] ) ) {
		return homeboy_datamachine_agent_result( array( 'import_succeeded' => 0, 'import_elapsed_ms' => $import_elapsed_ms ), $metadata, 'datamachine/import-agent did not succeed' );
	}

	$agent = $agents->get_by_slug( $agent_slug );
	if ( ! $agent ) {
		return homeboy_datamachine_agent_result( array( 'agent_resolved' => 0 ), $metadata, 'Imported agent was not found' );
	}

	$agent_id = (int) $agent['agent_id'];
	$agent_config = is_array( $agent['agent_config'] ?? null ) ? $agent['agent_config'] : array();
	$agent_config['default_provider'] = $settings['default_provider'];
	$agent_config['default_model']    = $settings['default_model'];
	$agent_config['mode_models']      = $settings['mode_models'];
	$agents->update_agent( $agent_id, array( 'agent_config' => $agent_config ) );
	PluginSettings::clearCache();

    $workflow_payload_path = str_starts_with( $execute_workflow_path, '/' ) ? $execute_workflow_path : rtrim( homeboy_datamachine_agent_scalar( $config, 'component_path' ), '/' ) . '/' . ltrim( $execute_workflow_path, '/' );
    if ( ! is_file( $workflow_payload_path ) ) {
        return homeboy_datamachine_agent_result( array( 'execute_workflow_payload_exists' => 0 ), $metadata, 'execute_workflow_path does not exist: ' . $execute_workflow_path );
    }
    $workflow_payload = json_decode( file_get_contents( $workflow_payload_path ) ?: '', true );
    if ( ! is_array( $workflow_payload ) ) {
        return homeboy_datamachine_agent_result( array( 'execute_workflow_payload_valid' => 0 ), $metadata, 'execute_workflow_path did not contain a JSON object: ' . $execute_workflow_path );
    }

    $execute_input = isset( $workflow_payload['workflow'] ) ? $workflow_payload : array( 'workflow' => $workflow_payload );
    if ( ! isset( $execute_input['initial_data'] ) || ! is_array( $execute_input['initial_data'] ) ) {
        $execute_input['initial_data'] = array();
    }
    $execute_input['initial_data']['agent_slug'] = $execute_input['initial_data']['agent_slug'] ?? $agent_slug;
    $execute_input['initial_data']['agent_id'] = $execute_input['initial_data']['agent_id'] ?? $agent_id;
    $execute_input['initial_data']['job_source'] = $execute_input['initial_data']['job_source'] ?? 'system';
    $execute_input['initial_data']['job_label'] = $execute_input['initial_data']['job_label'] ?? 'Data Machine agent workflow';

    $run_start = hrtime( true );
    $run_result = wp_get_ability( 'datamachine/execute-workflow' )->execute( $execute_input );
    $run_elapsed_ms = ( hrtime( true ) - $run_start ) / 1000000;
    $metadata['run_result'] = $run_result;
    $job_id = is_array( $run_result ) ? (int) ( $run_result['job_id'] ?? 0 ) : 0;
    if ( ! is_array( $run_result ) || empty( $run_result['success'] ) || $job_id <= 0 ) {
        return homeboy_datamachine_agent_result( array( 'execute_workflow_succeeded' => 0, 'run_elapsed_ms' => $run_elapsed_ms ), $metadata, 'datamachine/execute-workflow failed or returned no job_id' );
    }
} else {
    $import_start = hrtime( true );
    $import_result = wp_get_ability( 'datamachine/import-agent' )->execute(
        array(
            'source'      => $bundle_path,
            'on_conflict' => homeboy_datamachine_agent_scalar( $config, 'on_conflict', 'skip' ),
        )
    );
    $import_elapsed_ms = ( hrtime( true ) - $import_start ) / 1000000;
    $metadata['import_result'] = $import_result;
    if ( ! is_array( $import_result ) || empty( $import_result['success'] ) ) {
        return homeboy_datamachine_agent_result( array( 'import_succeeded' => 0, 'import_elapsed_ms' => $import_elapsed_ms ), $metadata, 'datamachine/import-agent did not succeed' );
    }

    $agent = $agents->get_by_slug( $agent_slug );
    if ( ! $agent ) {
        return homeboy_datamachine_agent_result( array( 'agent_resolved' => 0 ), $metadata, 'Imported agent was not found' );
    }

    $agent_id = (int) $agent['agent_id'];
    $agent_config = is_array( $agent['agent_config'] ?? null ) ? $agent['agent_config'] : array();
    $agent_config['default_provider'] = $settings['default_provider'];
    $agent_config['default_model']    = $settings['default_model'];
    $agent_config['mode_models']      = $settings['mode_models'];
    $agents->update_agent( $agent_id, array( 'agent_config' => $agent_config ) );
    PluginSettings::clearCache();

    if ( '' !== $pipeline_slug ) {
        $pipeline = $pipelines->get_by_portable_slug( $agent_id, $pipeline_slug );
        if ( ! $pipeline ) {
            return homeboy_datamachine_agent_result( array( 'pipeline_resolved' => 0 ), $metadata + array( 'agent_id' => $agent_id ), 'Imported pipeline was not found' );
        }
        $pipeline_id = (int) $pipeline['pipeline_id'];
        $pipeline_step_patches = is_array( $config['pipeline_step_patches'] ?? null ) ? $config['pipeline_step_patches'] : array();
        if ( ! empty( $pipeline_step_patches ) ) {
            $pipeline_config = is_array( $pipeline['pipeline_config'] ?? null ) ? $pipeline['pipeline_config'] : array();
            $pipeline_config = homeboy_datamachine_agent_apply_step_patches( $pipeline_config, $pipeline_step_patches );
            $pipelines->update_pipeline( $pipeline_id, array( 'pipeline_config' => $pipeline_config ) );
        }
    }

    $flow = $flows->get_by_portable_slug( $pipeline_id, $flow_slug );
    if ( ! $flow && 0 !== $pipeline_id ) {
        $flow = $flows->get_by_portable_slug( 0, $flow_slug );
    }
    if ( ! $flow ) {
        return homeboy_datamachine_agent_result( array( 'flow_resolved' => 0 ), $metadata + array( 'agent_id' => $agent_id, 'pipeline_id' => $pipeline_id ), 'Imported flow was not found' );
    }

    $flow_id = (int) $flow['flow_id'];
    $flow_config = is_array( $flow['flow_config'] ?? null ) ? $flow['flow_config'] : array();
    $flow_config = homeboy_datamachine_agent_apply_step_patches( $flow_config, is_array( $config['flow_step_patches'] ?? null ) ? $config['flow_step_patches'] : array() );
    foreach ( $flow_config as &$step_config ) {
        if ( 'ai' === (string) ( $step_config['step_type'] ?? '' ) ) {
            $step_config['prompt_queue'] = homeboy_datamachine_agent_run_prompt_queue( $step_config, $prompt );
            $step_config['queue_mode'] = 'static';
        }
    }
    unset( $step_config );
    $flows->update_flow( $flow_id, array( 'flow_config' => $flow_config, 'agent_id' => $agent_id ) );

    $run_start = hrtime( true );
    $run_result = wp_get_ability( 'datamachine/run-flow' )->execute( array( 'flow_id' => $flow_id ) );
    $run_elapsed_ms = ( hrtime( true ) - $run_start ) / 1000000;
    $metadata['run_result'] = $run_result;
    $job_id = is_array( $run_result ) ? (int) ( $run_result['job_id'] ?? 0 ) : 0;
    if ( ! is_array( $run_result ) || empty( $run_result['success'] ) || $job_id <= 0 ) {
        return homeboy_datamachine_agent_result( array( 'run_flow_succeeded' => 0, 'run_elapsed_ms' => $run_elapsed_ms ), $metadata, 'datamachine/run-flow failed or returned no job_id' );
    }
}

$drain_summary = homeboy_datamachine_agent_drain_job( $job_id, $config, $jobs );
$drain_result = $drain_summary['drain_result'];
$drain_elapsed_ms = (float) $drain_summary['drain_elapsed_ms'];
$metadata['drain_result'] = $drain_result;
$metadata['drain_history'] = $drain_summary['drain_history'];
$metadata['retry_waited_ms'] = $drain_summary['retry_waited_ms'];

$child_drain_summary = homeboy_datamachine_agent_drain_child_jobs( $job_id, $config, $jobs );
$metadata['child_jobs'] = array_map(
    static fn( array $child_job ) => array(
        'job_id'        => (int) ( $child_job['job_id'] ?? 0 ),
        'status'        => (string) ( $child_job['status'] ?? '' ),
        'parent_job_id' => (int) ( $child_job['parent_job_id'] ?? 0 ),
    ),
    $child_drain_summary['children']
);
$metadata['child_drain_results'] = $child_drain_summary['drain_results'];

$job = $jobs->get_job( $job_id );
$job_status = is_array( $job ) ? (string) ( $job['status'] ?? '' ) : '';
$engine_data = function_exists( 'datamachine_get_engine_data' ) ? datamachine_get_engine_data( $job_id ) : array();
$engine_data = homeboy_datamachine_agent_merge_recorded_tool_results( $engine_data, $config );
$engine_data = homeboy_datamachine_agent_merge_child_engine_data( $engine_data, $child_drain_summary['children'], $config );
$tool_audit_events = homeboy_datamachine_agent_tool_audit_events( $engine_data, $config );
$runner_workspace_capture = homeboy_datamachine_agent_capture_runner_workspace( $engine_data, $config );
if ( is_array( $runner_workspace_capture['engine_data'] ?? null ) ) {
	$engine_data = $runner_workspace_capture['engine_data'];
}
$transcript_dir = homeboy_datamachine_agent_scalar( $config, 'transcript_dir' );
$transcript_artifacts = homeboy_datamachine_agent_export_transcript( $job_id, $engine_data, $transcript_dir );
$pr_opened = homeboy_datamachine_agent_pr_opened( $engine_data, $config );
$file_written = homeboy_datamachine_agent_file_written( $engine_data, $config ) || ! empty( $runner_workspace_capture['changed'] );
$fallback_pull_request = array( 'opened' => false );
$success_requires_pr = ! empty( $config['success_requires_pr'] );
if ( $file_written && ! $pr_opened && empty( $runner_workspace_capture['changed'] ) ) {
    $fallback_pull_request = homeboy_datamachine_agent_open_fallback_pr( $engine_data, $config );
    if ( ! empty( $fallback_pull_request['opened'] ) && is_array( $fallback_pull_request['engine_data'] ?? null ) ) {
        $engine_data = $fallback_pull_request['engine_data'];
        $pr_opened   = true;
    }
}
$completion_outcome_satisfied = homeboy_datamachine_agent_completion_outcome_satisfied( $engine_data, $config );
$success_status = $pr_opened ? 'pr_opened' : ( $completion_outcome_satisfied ? 'completion_outcome_satisfied' : 'no_changes' );
$artifact_pr_context = array(
    'success_status'           => $success_status,
    'success_requires_pr'      => $success_requires_pr,
    'transcript_artifacts'     => $transcript_artifacts,
    'runner_workspace_capture' => $runner_workspace_capture,
    'fallback_pull_request'    => $fallback_pull_request,
    'error_message'            => (string) ( $engine_data['error_message'] ?? '' ),
);
if ( ! empty( $runner_workspace_capture['changed'] ) && ! $pr_opened ) {
    $runner_workspace = is_array( $config['runner_workspace_result'] ?? null ) ? $config['runner_workspace_result'] : array();
    $template_values  = homeboy_datamachine_agent_artifact_pr_context(
        $job_id,
        $config,
        $engine_data,
        array(),
        homeboy_datamachine_agent_runner_workspace_written_paths( $runner_workspace_capture ),
        $artifact_pr_context
    );
    $capture_config                          = $config;
    $capture_config['fallback_pull_request'] = homeboy_datamachine_agent_runner_workspace_fallback_config( $config, $runner_workspace, $template_values );
    $fallback_pull_request                   = homeboy_datamachine_agent_open_fallback_pr( $engine_data, $capture_config );
    if ( ! empty( $fallback_pull_request['opened'] ) && is_array( $fallback_pull_request['engine_data'] ?? null ) ) {
        $engine_data = $fallback_pull_request['engine_data'];
        $pr_opened   = true;
    }
    $artifact_pr_context['fallback_pull_request'] = $fallback_pull_request;
}
$job_artifact_exports = homeboy_datamachine_agent_export_job_artifacts( $job_id, $config, $pr_opened, $engine_data, $artifact_pr_context );
if ( is_array( $job_artifact_exports['engine_data'] ?? null ) ) {
    $engine_data = $job_artifact_exports['engine_data'];
    $pr_opened   = true;
}

$metadata += array(
    'agent_id'             => $agent_id,
    'pipeline_id'          => $pipeline_id,
    'flow_id'              => $flow_id,
    'job_id'               => $job_id,
    'job_status'           => $job_status,
	'engine_data'          => $engine_data,
	'tool_audit_events'   => $tool_audit_events,
	'transcript_session_id' => (string) ( $engine_data['transcript_session_id'] ?? '' ),
    'transcript_artifacts'  => $transcript_artifacts,
    'token_usage'           => is_array( $engine_data['token_usage'] ?? null ) ? $engine_data['token_usage'] : array(),
    'error_message'         => (string) ( $engine_data['error_message'] ?? '' ),
    'success_status'        => $success_status,
    'success_requires_pr'   => $success_requires_pr,
    'fallback_pull_request' => $fallback_pull_request,
    'runner_workspace_capture' => $runner_workspace_capture,
    'completion_outcome_satisfied' => $completion_outcome_satisfied,
    'file_written'          => $file_written,
    'job_artifact_exports'    => $job_artifact_exports,
);

$metadata['general_rule_results'] = homeboy_datamachine_agent_evaluate_general_rules( $metadata, $config );
$general_rule_failures = array();
foreach ( $metadata['general_rule_results'] as $rule_result ) {
    if ( ! is_array( $rule_result ) || 'failed' !== (string) ( $rule_result['status'] ?? '' ) ) {
        continue;
    }
    $general_rule_failures = array_merge( $general_rule_failures, homeboy_datamachine_agent_normalized_list( $rule_result['failure_reasons'] ?? array() ) );
}
if ( ! empty( $general_rule_failures ) ) {
    $metadata['failure_reasons'] = array_values( array_unique( array_merge( homeboy_datamachine_agent_normalized_list( $metadata['failure_reasons'] ?? array() ), $general_rule_failures ) ) );
}

if ( ! empty( $runner_workspace_capture['enabled'] ) && ! empty( $runner_workspace_capture['error'] ) && empty( $runner_workspace_capture['changed'] ) ) {
    return homeboy_datamachine_agent_result( array( 'runner_workspace_captured' => 0 ), $metadata, (string) $runner_workspace_capture['error'] );
}

if ( $file_written && ! $pr_opened ) {
    $metadata['success_status'] = 'write_without_pr';
    $fallback_error = is_array( $fallback_pull_request ) ? (string) ( $fallback_pull_request['error'] ?? '' ) : '';
    $capture_error  = is_array( $runner_workspace_capture ) ? (string) ( $runner_workspace_capture['error'] ?? '' ) : '';
    $error_message  = 'Agent wrote files without opening a pull request';
    if ( '' !== $fallback_error ) {
        $error_message .= ': ' . $fallback_error;
    } elseif ( '' !== $capture_error ) {
        $error_message .= ': ' . $capture_error;
    }
    return homeboy_datamachine_agent_result( array( 'file_written' => 1, 'pr_opened' => 0 ), $metadata, $error_message );
}

if ( ! empty( $job_artifact_exports['error'] ) ) {
    return homeboy_datamachine_agent_result( array( 'job_artifact_exported' => 0 ), $metadata, (string) $job_artifact_exports['error'] );
}

if ( $success_requires_pr && ! $pr_opened && ! $completion_outcome_satisfied ) {
    return homeboy_datamachine_agent_result( array( 'pr_opened' => 0 ), $metadata, 'Agent completed without opening a pull request' );
}

return homeboy_datamachine_agent_result(
	array(
		'config_present'              => 1,
		'required_abilities_resolved' => 1,
		'required_classes_available'  => 1,
		'bundle_exists'               => 1,
		'import_succeeded'            => 1,
		'execute_workflow_succeeded'  => '' !== $execute_workflow_path ? 1 : 0,
		'import_elapsed_ms'           => $import_elapsed_ms,
		'agent_resolved'              => 1,
		'runner_workspace_provisioned' => empty( $runner_workspace['enabled'] ) || ! empty( $runner_workspace['success'] ) ? 1 : 0,
		'runner_workspace_captured'    => empty( $runner_workspace_capture['enabled'] ) || empty( $runner_workspace_capture['error'] ) ? 1 : 0,
		'pipeline_resolved'           => '' !== $execute_workflow_path || '' === $pipeline_slug || $pipeline_id > 0 ? 1 : 0,
		'flow_resolved'               => 1,
		'run_flow_succeeded'          => 1,
		'run_elapsed_ms'              => $run_elapsed_ms,
		'drain_succeeded'             => is_array( $drain_result ) && ! empty( $drain_result['success'] ) ? 1 : 0,
		'drain_elapsed_ms'            => $drain_elapsed_ms,
		'job_completed'               => 'completed' === $job_status ? 1 : 0,
		'file_written'                => $file_written ? 1 : 0,
		'pr_opened'                   => $pr_opened ? 1 : 0,
		'completion_outcome_satisfied' => $completion_outcome_satisfied ? 1 : 0,
		'no_changes'                  => ! $file_written && ! $pr_opened && ! $completion_outcome_satisfied ? 1 : 0,
		'job_artifact_exported'       => ! empty( $job_artifact_exports['pr_url'] ) ? 1 : 0,
		'transcript_exported'         => ! empty( $transcript_artifacts['json'] ) ? 1 : 0,
		'total_tokens'                => (int) ( $metadata['token_usage']['total_tokens'] ?? 0 ),
	),
	$metadata
);
