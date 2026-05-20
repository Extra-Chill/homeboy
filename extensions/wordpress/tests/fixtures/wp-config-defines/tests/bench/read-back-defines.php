<?php
/**
 * wp-config-defines smoke workload — reads back constants the dispatcher
 * injected via the wp_config_defines setting and proves type preservation.
 *
 * The fixture's homeboy.json declares:
 *
 *     extensions.wordpress.settings.wp_config_defines = {
 *         "WP_CONFIG_FIXTURE_STRING": "hello",
 *         "WP_CONFIG_FIXTURE_INT": 42,
 *         "WP_CONFIG_FIXTURE_BOOL": true
 *     }
 *
 * After WP Codebox boots the WordPress runtime, those should be defined as PHP
 * constants with their original types (var_export round-trip).
 *
 * Writes the read-back values to <shared-state>/defines-read-back.log so
 * the smoke script can grep for them outside Playground.
 */
return function (): array {
    $shared = defined('HOMEBOY_BENCH_SHARED_STATE') ? HOMEBOY_BENCH_SHARED_STATE : '';

    $values = [
        'string' => defined('WP_CONFIG_FIXTURE_STRING') ? WP_CONFIG_FIXTURE_STRING : '<undefined>',
        'int' => defined('WP_CONFIG_FIXTURE_INT') ? WP_CONFIG_FIXTURE_INT : '<undefined>',
        'bool' => defined('WP_CONFIG_FIXTURE_BOOL') ? WP_CONFIG_FIXTURE_BOOL : '<undefined>',
    ];

    $types = [
        'string' => defined('WP_CONFIG_FIXTURE_STRING') ? gettype(WP_CONFIG_FIXTURE_STRING) : '<undefined>',
        'int' => defined('WP_CONFIG_FIXTURE_INT') ? gettype(WP_CONFIG_FIXTURE_INT) : '<undefined>',
        'bool' => defined('WP_CONFIG_FIXTURE_BOOL') ? gettype(WP_CONFIG_FIXTURE_BOOL) : '<undefined>',
    ];

    if ($shared !== '') {
        $log_path = $shared . '/defines-read-back.log';
        $line = sprintf(
            "string=%s int=%s bool=%s string_type=%s int_type=%s bool_type=%s\n",
            var_export($values['string'], true),
            var_export($values['int'], true),
            var_export($values['bool'], true),
            $types['string'],
            $types['int'],
            $types['bool']
        );
        file_put_contents($log_path, $line, FILE_APPEND | LOCK_EX);
    }

    return [
        'kind' => 'wp-config-defines-read-back',
        'values' => $values,
        'types' => $types,
    ];
};
