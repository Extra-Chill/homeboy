<?php
/**
 * bench_env smoke workload — reads back env vars the dispatcher injected
 * via the bench_env setting and asserts they're visible to getenv().
 *
 * The fixture's homeboy.json (or a synthetic HOMEBOY_SETTINGS_JSON in
 * the smoke script) declares:
 *
 *     extensions.wordpress.settings.bench_env = {
 *         "BENCH_ENV_FIXTURE_STR": "hello",
 *         "BENCH_ENV_FIXTURE_NUM": "42"
 *     }
 *
 * After the runner template's putenv() loop runs, those should be
 * readable via getenv() inside the workload.
 *
 * Writes the read-back values to <shared-state>/env-read-back.log so the
 * smoke script can grep for them outside Playground.
 */
return function (): array {
    $shared = defined('HOMEBOY_BENCH_SHARED_STATE') ? HOMEBOY_BENCH_SHARED_STATE : '';

    // BENCH_ENV_FIXTURE_METAS optionally carries a JSON value with `\` and
    // `&` embedded — chars that GNU sed mangles in `s` replacement strings
    // unless the dispatcher escapes them before substituting BENCH_ENV_JSON
    // into the runner template (homeboy-extensions sed-escape fix). The
    // smoke script grep-asserts these survive the Playground boundary.
    $values = [
        'BENCH_ENV_FIXTURE_STR_getenv' => var_export(getenv('BENCH_ENV_FIXTURE_STR'), true),
        'BENCH_ENV_FIXTURE_NUM_getenv' => var_export(getenv('BENCH_ENV_FIXTURE_NUM'), true),
        'BENCH_ENV_FIXTURE_STR_in_env' => array_key_exists('BENCH_ENV_FIXTURE_STR', $_ENV) ? 'yes' : 'no',
        'BENCH_ENV_FIXTURE_STR_env_value' => $_ENV['BENCH_ENV_FIXTURE_STR'] ?? '<missing>',
        'BENCH_ENV_FIXTURE_METAS_getenv' => var_export(getenv('BENCH_ENV_FIXTURE_METAS'), true),
    ];

    if ($shared !== '') {
        $log_path = $shared . '/env-read-back.log';
        $line = json_encode($values) . "\n";
        file_put_contents($log_path, $line, FILE_APPEND | LOCK_EX);
    }

    return [
        'kind' => 'bench-env-read-back',
        'values' => $values,
    ];
};
