<?php
/**
 * Echoes the Data Machine workload return value for WP Codebox code-file runs.
 */

$homeboy_workload_result = require '/homeboy-extension/scripts/agent/datamachine-agent-workload.php';
echo wp_json_encode( is_array( $homeboy_workload_result ) ? $homeboy_workload_result : array() );
