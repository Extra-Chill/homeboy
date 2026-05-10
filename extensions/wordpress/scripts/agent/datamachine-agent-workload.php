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
use DataMachine\Core\PluginSettings;

if ( ! function_exists( 'homeboy_datamachine_agent_result' ) ) {
    function homeboy_datamachine_agent_result( array $metrics, array $metadata, ?string $error = null ): array {
        if ( null !== $error ) {
            $metadata['error'] = $error;
        }

        return array(
            'metrics'  => $metrics,
            'metadata' => $metadata,
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
    function homeboy_datamachine_agent_pr_opened( array $engine_data, array $config ): bool {
        $tool_results_key = homeboy_datamachine_agent_scalar( $config, 'tool_results_key', 'github_tool_results' );
        $tool_results     = is_array( $engine_data[ $tool_results_key ] ?? null ) ? $engine_data[ $tool_results_key ] : array();

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
            if ( '' === $engine_key || $job_id <= 0 || ! function_exists( 'datamachine_merge_engine_data' ) ) {
                return;
            }

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
                    'url'       => homeboy_datamachine_agent_first_url( $response ),
                    'error'     => (string) ( $response['error'] ?? '' ),
                    'message'   => (string) ( $response['message'] ?? '' ),
                );
                $payload[ (string) $record['tool_results_key'] ] = self::$tool_results;
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

if ( ! function_exists( 'homeboy_datamachine_agent_register_tool_recorders' ) ) {
    function homeboy_datamachine_agent_register_tool_recorders( array $config ): void {
        $ability_tools = is_array( $config['ability_tools'] ?? null ) ? $config['ability_tools'] : array();
        $recorders     = is_array( $config['tool_recorders'] ?? null ) ? $config['tool_recorders'] : array();
        if ( empty( $ability_tools ) && empty( $recorders ) ) {
            return;
        }

        add_filter(
            'datamachine_resolved_tools',
            static function ( array $tools ) use ( $ability_tools, $recorders ): array {
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
        $target_repo      = homeboy_datamachine_agent_scalar( $config, 'target_repo' );
        if ( '' !== $github_token && '' !== $target_repo ) {
            $allowed_repos = is_array( $config['allowed_repos'] ?? null ) ? $config['allowed_repos'] : array( $target_repo );
            $profile_id    = homeboy_datamachine_agent_scalar( $config, 'github_profile_id', 'homeboy-agent-ci' );
            $settings['github_credential_profiles'] = array(
                array(
                    'id'            => $profile_id,
                    'label'         => 'Homeboy agent CI token',
                    'mode'          => 'pat',
                    'pat'           => $github_token,
                    'default_repo'  => $target_repo,
                    'allowed_repos' => array_values( array_unique( array_filter( array_map( 'strval', $allowed_repos ) ) ) ),
                ),
            );
            $settings['github_default_profile_id'] = $profile_id;
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

        $path = rtrim( $transcript_dir, '/' ) . '/job-' . $job_id . '-transcript.json';
        file_put_contents(
            $path,
            wp_json_encode(
                array(
                    'job_id'     => $job_id,
                    'session_id' => $session_id,
                    'provider'   => $session['provider'] ?? null,
                    'model'      => $session['model'] ?? null,
                    'metadata'   => is_array( $session['metadata'] ?? null ) ? $session['metadata'] : array(),
                    'messages'   => is_array( $session['messages'] ?? null ) ? $session['messages'] : array(),
                ),
                JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE
            )
        );

        return array(
            'session_id' => $session_id,
            'json'       => $path,
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
            'agent_slug'          => homeboy_datamachine_agent_scalar( $config, 'agent_slug' ),
            'flow_slug'           => homeboy_datamachine_agent_scalar( $config, 'flow_slug' ),
            'provider'            => homeboy_datamachine_agent_scalar( $config, 'provider', 'openai' ),
            'model'               => homeboy_datamachine_agent_scalar( $config, 'model', 'gpt-5.5' ),
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
    'bundle_exists' => '' !== $bundle_path && is_dir( $bundle_path ),
);

foreach ( array( 'bundle_path' => $bundle_path, 'agent_slug' => $agent_slug, 'flow_slug' => $flow_slug ) as $label => $value ) {
    if ( '' === $value ) {
        return homeboy_datamachine_agent_result( array( $label . '_present' => 0 ), $metadata, $label . ' is required' );
    }
}
if ( '' === $prompt ) {
    return homeboy_datamachine_agent_result( array( 'prompt_present' => 0 ), $metadata, 'prompt or prompt_env is required' );
}
if ( ! $metadata['bundle_exists'] || ! is_file( $bundle_path . '/manifest.json' ) ) {
    return homeboy_datamachine_agent_result( array( 'bundle_exists' => 0 ), $metadata, 'Agent bundle directory missing or incomplete' );
}

homeboy_datamachine_agent_bootstrap_provider( $config );
$bootstrap_error = homeboy_datamachine_agent_bootstrap_abilities();
if ( null !== $bootstrap_error ) {
    return $bootstrap_error;
}

$required_abilities = is_array( $config['required_abilities'] ?? null ) ? $config['required_abilities'] : array( 'datamachine/import-agent', 'datamachine/run-flow', 'datamachine/drain-job' );
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
homeboy_datamachine_agent_register_tool_recorders( $config );

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

$agents    = new Agents();
$pipelines = new Pipelines();
$flows     = new Flows();
$jobs      = new Jobs();

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

$pipeline_id = 0;
$pipeline_slug = homeboy_datamachine_agent_scalar( $config, 'pipeline_slug' );
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
        $step_config['prompt_queue'] = array(
            array(
                'prompt'   => $prompt,
                'added_at' => gmdate( 'c' ),
            ),
        );
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

$drain_summary = homeboy_datamachine_agent_drain_job( $job_id, $config, $jobs );
$drain_result = $drain_summary['drain_result'];
$drain_elapsed_ms = (float) $drain_summary['drain_elapsed_ms'];
$metadata['drain_result'] = $drain_result;
$metadata['drain_history'] = $drain_summary['drain_history'];
$metadata['retry_waited_ms'] = $drain_summary['retry_waited_ms'];

$job = $jobs->get_job( $job_id );
$job_status = is_array( $job ) ? (string) ( $job['status'] ?? '' ) : '';
$engine_data = function_exists( 'datamachine_get_engine_data' ) ? datamachine_get_engine_data( $job_id ) : array();
$transcript_dir = homeboy_datamachine_agent_scalar( $config, 'transcript_dir' );
$transcript_artifacts = homeboy_datamachine_agent_export_transcript( $job_id, $engine_data, $transcript_dir );
$pr_opened = homeboy_datamachine_agent_pr_opened( $engine_data, $config );
$success_status = $pr_opened ? 'pr_opened' : 'no_changes';
$success_requires_pr = ! empty( $config['success_requires_pr'] );

$metadata += array(
    'agent_id'             => $agent_id,
    'pipeline_id'          => $pipeline_id,
    'flow_id'              => $flow_id,
    'job_id'               => $job_id,
    'job_status'           => $job_status,
    'engine_data'          => $engine_data,
    'transcript_session_id' => (string) ( $engine_data['transcript_session_id'] ?? '' ),
    'transcript_artifacts'  => $transcript_artifacts,
    'token_usage'           => is_array( $engine_data['token_usage'] ?? null ) ? $engine_data['token_usage'] : array(),
    'error_message'         => (string) ( $engine_data['error_message'] ?? '' ),
    'success_status'        => $success_status,
    'success_requires_pr'   => $success_requires_pr,
);

if ( $success_requires_pr && ! $pr_opened ) {
    return homeboy_datamachine_agent_result( array( 'pr_opened' => 0 ), $metadata, 'Agent completed without opening a pull request' );
}

return homeboy_datamachine_agent_result(
    array(
        'config_present'              => 1,
        'required_abilities_resolved' => 1,
        'required_classes_available'  => 1,
        'bundle_exists'               => 1,
        'import_succeeded'            => 1,
        'import_elapsed_ms'           => $import_elapsed_ms,
        'agent_resolved'              => 1,
        'pipeline_resolved'           => '' === $pipeline_slug || $pipeline_id > 0 ? 1 : 0,
        'flow_resolved'               => 1,
        'run_flow_succeeded'          => 1,
        'run_elapsed_ms'              => $run_elapsed_ms,
        'drain_succeeded'             => is_array( $drain_result ) && ! empty( $drain_result['success'] ) ? 1 : 0,
        'drain_elapsed_ms'            => $drain_elapsed_ms,
        'job_completed'               => 'completed' === $job_status ? 1 : 0,
        'pr_opened'                   => $pr_opened ? 1 : 0,
        'no_changes'                  => ! $pr_opened ? 1 : 0,
        'transcript_exported'         => ! empty( $transcript_artifacts['json'] ) ? 1 : 0,
        'total_tokens'                => (int) ( $metadata['token_usage']['total_tokens'] ?? 0 ),
    ),
    $metadata
);
