<?php
/** Workload that exercises the generic WordPress bench crawl helper. */

require_once '/homeboy-extension/scripts/bench/lib/wordpress-bench-crawl.php';

return function (): array {
    return homeboy_wordpress_bench_crawl_payload(
        [
            '/',
            '/__homeboy-bench-crawl-helper-missing',
            '/__homeboy-bench-crawl-helper-skipped-by-bound',
        ],
        [
            'batch_index' => 7,
            'max_requests' => 2,
            'timeout' => 10,
        ]
    );
};
