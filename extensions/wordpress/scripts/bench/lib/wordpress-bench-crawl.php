<?php
/**
 * Generic WordPress URL crawl helper for bench workloads.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/wordpress-bench-crawl.php
 */

if (!function_exists('homeboy_wordpress_bench_crawl_normalize_entries')) {
    /**
     * Normalize string/object URL entries while preserving order.
     *
     * @param array<int,mixed> $entries Ordered crawl entries.
     * @return array<int,array<string,mixed>> Normalized crawl entries.
     */
    function homeboy_wordpress_bench_crawl_normalize_entries(array $entries): array {
        $normalized = [];

        foreach ($entries as $entry) {
            if (is_string($entry)) {
                $normalized[] = ['route' => $entry];
                continue;
            }

            if (!is_array($entry)) {
                $normalized[] = [
                    'failure_message' => 'Crawl entry must be a string route/URL or an array.',
                ];
                continue;
            }

            $normalized[] = $entry;
        }

        return $normalized;
    }
}

if (!function_exists('homeboy_wordpress_bench_crawl_url_for_entry')) {
    /**
     * Resolve a crawl entry into a URL and optional route label.
     *
     * @param array<string,mixed> $entry Crawl entry.
     * @return array{url:string,route:string|null,failure_message:string|null}
     */
    function homeboy_wordpress_bench_crawl_url_for_entry(array $entry): array {
        $route = null;
        $url = '';

        if (isset($entry['url']) && is_scalar($entry['url'])) {
            $url = trim((string) $entry['url']);
        } elseif (isset($entry['route']) && is_scalar($entry['route'])) {
            $route = trim((string) $entry['route']);
            $url = $route;
        }

        if ($url === '') {
            return [
                'url' => '',
                'route' => $route,
                'failure_message' => isset($entry['failure_message']) && is_string($entry['failure_message'])
                    ? $entry['failure_message']
                    : 'Crawl entry is missing a url or route.',
            ];
        }

        if (preg_match('#^https?://#i', $url) !== 1) {
            $route = $route ?? $url;
            $url = function_exists('home_url') ? home_url($route) : $url;
        }

        return [
            'url' => $url,
            'route' => $route,
            'failure_message' => null,
        ];
    }
}

if (!function_exists('homeboy_wordpress_bench_crawl_request_args')) {
    /**
     * Build WordPress HTTP API arguments for a crawl entry.
     *
     * @param array<string,mixed> $entry Crawl entry.
     * @param array<string,mixed> $options Crawl options.
     * @return array<string,mixed> Request arguments.
     */
    function homeboy_wordpress_bench_crawl_request_args(array $entry, array $options): array {
        $method = isset($entry['method']) && is_scalar($entry['method'])
            ? strtoupper((string) $entry['method'])
            : strtoupper((string) ($options['method'] ?? 'GET'));
        $timeout = isset($entry['timeout']) && is_numeric($entry['timeout'])
            ? (float) $entry['timeout']
            : (float) ($options['timeout'] ?? 15);

        $args = [
            'method' => $method,
            'timeout' => $timeout,
            'redirection' => isset($options['redirection']) && is_numeric($options['redirection'])
                ? (int) $options['redirection']
                : 5,
        ];

        if (isset($entry['headers']) && is_array($entry['headers'])) {
            $args['headers'] = $entry['headers'];
        } elseif (isset($options['headers']) && is_array($options['headers'])) {
            $args['headers'] = $options['headers'];
        }

        if (array_key_exists('body', $entry)) {
            $args['body'] = $entry['body'];
        }

        return $args;
    }
}

if (!function_exists('homeboy_wordpress_bench_crawl')) {
    /**
     * Crawl a bounded ordered list of WordPress URLs/routes and return rows.
     *
     * Supported entries:
     * - '/route/'
     * - 'https://example.test/absolute-url/'
     * - ['route' => '/route/', 'method' => 'GET']
     * - ['url' => 'https://example.test/absolute-url/', 'headers' => [...]]
     *
     * Supported options:
     * - batch_index: batch number attached to each row. Defaults to 0.
     * - max_requests: upper bound on crawled entries. Defaults to the list length.
     * - timeout: per-request timeout in seconds. Defaults to 15.
     * - method: default HTTP method. Defaults to GET.
     * - include_response_bytes: attach strlen(body) when true. Defaults to true.
     *
     * @param array<int,mixed>    $entries Ordered crawl entries.
     * @param array<string,mixed> $options Crawl options.
     * @return array<int,array<string,mixed>> Per-request crawl rows.
     */
    function homeboy_wordpress_bench_crawl(array $entries, array $options = []): array {
        $normalized = homeboy_wordpress_bench_crawl_normalize_entries($entries);
        $max_requests = isset($options['max_requests']) && is_numeric($options['max_requests'])
            ? max(0, (int) $options['max_requests'])
            : count($normalized);
        $batch_index = isset($options['batch_index']) && is_numeric($options['batch_index'])
            ? (int) $options['batch_index']
            : 0;
        $include_response_bytes = array_key_exists('include_response_bytes', $options)
            ? (bool) $options['include_response_bytes']
            : true;

        $rows = [];
        $bounded_entries = array_slice($normalized, 0, $max_requests);

        foreach ($bounded_entries as $request_index => $entry) {
            $resolved = homeboy_wordpress_bench_crawl_url_for_entry($entry);
            $args = homeboy_wordpress_bench_crawl_request_args($entry, $options);
            $row = [
                'batch_index' => $batch_index,
                'request_index' => $request_index,
                'url' => $resolved['url'],
                'method' => $args['method'],
                'status' => 'not_requested',
                'http_status' => null,
                'elapsed_ms' => 0.0,
            ];
            if ($resolved['route'] !== null) {
                $row['route'] = $resolved['route'];
            }

            if ($resolved['failure_message'] !== null) {
                $row['status'] = 'request_error';
                $row['failure_message'] = $resolved['failure_message'];
                $rows[] = $row;
                continue;
            }

            $started = microtime(true);
            $response = wp_remote_request($resolved['url'], $args);
            $row['elapsed_ms'] = round((microtime(true) - $started) * 1000, 3);

            if (is_wp_error($response)) {
                $row['status'] = 'request_error';
                $row['failure_message'] = $response->get_error_message();
                $rows[] = $row;
                continue;
            }

            $http_status = (int) wp_remote_retrieve_response_code($response);
            $row['http_status'] = $http_status;
            $row['status'] = $http_status >= 200 && $http_status < 400 ? 'ok' : 'http_error';

            if ($include_response_bytes) {
                $body = wp_remote_retrieve_body($response);
                $row['response_bytes'] = is_string($body) ? strlen($body) : null;
            }
            if ($row['status'] === 'http_error') {
                $row['failure_message'] = 'HTTP ' . $http_status;
            }

            $rows[] = $row;
        }

        return $rows;
    }
}

if (!function_exists('homeboy_wordpress_bench_crawl_payload')) {
    /**
     * Return a bench payload with numeric crawl metrics and structured rows.
     *
     * @param array<int,mixed>    $entries Ordered crawl entries.
     * @param array<string,mixed> $options Crawl options.
     * @return array<string,array<string,mixed>> Workload payload.
     */
    function homeboy_wordpress_bench_crawl_payload(array $entries, array $options = []): array {
        $rows = homeboy_wordpress_bench_crawl($entries, $options);
        $metrics = [
            'crawl_requests' => count($rows),
            'crawl_successes' => 0,
            'crawl_failures' => 0,
            'crawl_2xx' => 0,
            'crawl_3xx' => 0,
            'crawl_4xx' => 0,
            'crawl_5xx' => 0,
            'crawl_elapsed_ms_total' => 0.0,
            'crawl_response_bytes_total' => 0,
        ];

        foreach ($rows as $row) {
            $status = isset($row['status']) ? (string) $row['status'] : '';
            if ($status === 'ok') {
                $metrics['crawl_successes']++;
            } else {
                $metrics['crawl_failures']++;
            }

            $http_status = isset($row['http_status']) && is_numeric($row['http_status']) ? (int) $row['http_status'] : 0;
            if ($http_status >= 200 && $http_status < 300) {
                $metrics['crawl_2xx']++;
            } elseif ($http_status >= 300 && $http_status < 400) {
                $metrics['crawl_3xx']++;
            } elseif ($http_status >= 400 && $http_status < 500) {
                $metrics['crawl_4xx']++;
            } elseif ($http_status >= 500 && $http_status < 600) {
                $metrics['crawl_5xx']++;
            }

            $metrics['crawl_elapsed_ms_total'] += isset($row['elapsed_ms']) && is_numeric($row['elapsed_ms']) ? (float) $row['elapsed_ms'] : 0.0;
            $metrics['crawl_response_bytes_total'] += isset($row['response_bytes']) && is_numeric($row['response_bytes']) ? (int) $row['response_bytes'] : 0;
        }

        return [
            'metrics' => $metrics,
            'metadata' => [
                'wordpress_bench_crawl' => [
                    'schema' => 'homeboy/wordpress-bench-crawl/v1',
                    'rows' => $rows,
                ],
            ],
        ];
    }
}
