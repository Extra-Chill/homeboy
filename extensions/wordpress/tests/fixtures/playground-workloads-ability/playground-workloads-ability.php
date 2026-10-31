<?php
/**
 * Plugin Name: Playground Workloads Ability Fixture
 *
 * Registers a tiny ability through the canonical WordPress Abilities API
 * (`wp_register_ability`, in core as of WP 6.9). The configured-workload
 * `ability` step calls into it via `wp_get_ability( $name )->execute( $input )`
 * to prove the runner can drive Abilities-API consumers without WP-CLI.
 *
 * Registration goes through the canonical `wp_abilities_api_init` action, the
 * same way real plugins register. Fixture has no compatibility shim: if the
 * Abilities API isn't loaded, the runner surfaces a typed RuntimeException.
 */

if (!function_exists('wp_register_ability')) {
    return;
}

add_action('wp_abilities_api_categories_init', static function (): void {
    if (!function_exists('wp_register_ability_category')) {
        return;
    }
    if (function_exists('wp_get_ability_category') && wp_get_ability_category('fixtures')) {
        return;
    }
    wp_register_ability_category('fixtures', [
        'label' => 'Fixtures',
        'description' => 'Abilities used by Playground bench fixtures.',
    ]);
});

add_action('wp_abilities_api_init', static function (): void {
    if (wp_get_ability('playground-workloads-fixture/run-pipeline')) {
        return;
    }
    wp_register_ability('playground-workloads-fixture/run-pipeline', [
        'label' => 'Playground Workloads Fixture: run pipeline',
        'description' => 'Test ability that emits metrics, artifacts, and metadata.',
        'category' => 'fixtures',
        'permission_callback' => static fn (): bool => true,
        'input_schema' => [
            'type' => 'object',
            'properties' => [
                'pipeline_id' => ['type' => 'integer'],
                'items' => ['type' => 'integer'],
            ],
        ],
        'output_schema' => [
            'type' => 'object',
            'properties' => [
                'metrics' => ['type' => 'object'],
                'artifacts' => ['type' => 'object'],
                'metadata' => ['type' => 'object'],
            ],
        ],
        'execute_callback' => static function (array $input = []) {
            $report_dir = WP_CONTENT_DIR . '/playground-workloads-fixture';
            if (!is_dir($report_dir)) {
                wp_mkdir_p($report_dir);
            }
            $report_path = $report_dir . '/ability-report.json';
            $items = isset($input['items']) ? max(0, (int) $input['items']) : 0;
            file_put_contents($report_path, wp_json_encode([
                'ok' => true,
                'items' => $items,
                'pipeline_id' => $input['pipeline_id'] ?? null,
            ]));

            return [
                'metrics' => [
                    'items_processed' => $items,
                ],
                'artifacts' => [
                    'ability_report' => [
                        'path' => 'wp-content/playground-workloads-fixture/ability-report.json',
                        'kind' => 'json',
                        'label' => 'Ability run report',
                    ],
                ],
                'metadata' => [
                    'pipeline_id' => $input['pipeline_id'] ?? null,
                    'phase' => 'ability-executed',
                ],
            ];
        },
    ]);
});
