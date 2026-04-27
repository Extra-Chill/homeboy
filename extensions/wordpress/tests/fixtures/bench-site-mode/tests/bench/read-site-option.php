<?php
/**
 * Reads a normal WordPress option so smoke tests can prove the runner booted
 * WordPress through the requested site mode.
 */
return function (): array {
    $site_title = function_exists('get_bloginfo') ? get_bloginfo('name') : '(wordpress not loaded)';

    if (defined('HOMEBOY_BENCH_SHARED_STATE') && HOMEBOY_BENCH_SHARED_STATE !== '') {
        file_put_contents(
            HOMEBOY_BENCH_SHARED_STATE . '/site-mode-read-back.log',
            'site_title=' . $site_title . "\n",
            FILE_APPEND
        );
    }

    return [
        'metrics' => ['title_length' => strlen($site_title)],
        'metadata' => ['site_title' => $site_title],
    ];
};
