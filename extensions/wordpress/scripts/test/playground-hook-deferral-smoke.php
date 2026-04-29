<?php
declare(strict_types=1);

$result_file = '/dev/null';
$current_stage = 'test';
require_once __DIR__ . '/../lib/playground-bootstrap.php';

$assertions = 0;
$assert_true = static function (bool $condition, string $message) use (&$assertions): void {
    ++$assertions;
    if (!$condition) {
        fwrite(STDERR, $message . "\n");
        exit(1);
    }
};

$calls = [];
$old_callback = static function () use (&$calls): void {
    $calls[] = 'old';
};
$new_callback = static function ($value) use (&$calls): void {
    $calls[] = 'new:' . $value;
};
$later_callback = static function () use (&$calls): void {
    $calls[] = 'later';
};

global $wp_filter;
$wp_filter = [
    'init' => (object) [
        'callbacks' => [
            10 => [
                'old_callback' => [
                    'function' => $old_callback,
                    'accepted_args' => 0,
                ],
            ],
        ],
    ],
];

$snapshot = pg_snapshot_wordpress_hook_callbacks('init');
$assert_true(isset($snapshot['10:old_callback']), 'snapshot should include existing callback');

$wp_filter['init']->callbacks[5]['new_callback'] = [
    'function' => $new_callback,
    'accepted_args' => 1,
];
$wp_filter['init']->callbacks[20]['later_callback'] = [
    'function' => $later_callback,
    'accepted_args' => 0,
];

$deferred = pg_defer_new_wordpress_hook_callbacks('init', $snapshot);
$assert_true(count($deferred) === 2, 'defer should extract only newly-added callbacks');
$assert_true(isset($wp_filter['init']->callbacks[10]['old_callback']), 'existing callback should remain registered');
$assert_true(!isset($wp_filter['init']->callbacks[5]), 'new priority bucket should be removed');
$assert_true(!isset($wp_filter['init']->callbacks[20]), 'later priority bucket should be removed');

pg_run_deferred_wordpress_hook_callbacks($deferred, ['value']);
$assert_true($calls === ['new:value', 'later'], 'deferred callbacks should run in priority order with accepted args');

$wp_filter['shutdown'] = (object) [
    'callbacks' => [
        10 => [
            'new_callback' => [
                'function' => $new_callback,
                'accepted_args' => 1,
            ],
        ],
    ],
];
pg_remove_new_wordpress_hook_callbacks('shutdown', []);
$assert_true($wp_filter['shutdown']->callbacks === [], 'remove helper should delete new callbacks without running them');
$assert_true($calls === ['new:value', 'later'], 'remove helper should not invoke callbacks');

$missing = pg_defer_new_wordpress_hook_callbacks('missing_hook', []);
$assert_true($missing === [], 'missing hook defer should be a no-op');

echo "Playground hook deferral smoke passed ({$assertions} assertions)\n";
