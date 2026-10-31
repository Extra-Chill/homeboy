<?php
/**
 * Plugin Name: Bench Plugins-Loaded Dep Fixture
 * Description: Validation-dependency fixture that registers via the canonical
 *              `plugins_loaded` hook. Used by the bench runner smoke test for
 *              homeboy-extensions#426 to prove a dep's `plugins_loaded`
 *              callback actually fires when the dep is loaded by the bench
 *              runner — i.e. that the dep's entry file was required BEFORE
 *              wp-settings.php fired `plugins_loaded`.
 *
 * Behavior:
 *   - On `plugins_loaded`, sets a global flag and registers an ability via
 *     `wp_register_ability` (gated by the canonical `wp_abilities_api_init`
 *     action).
 *   - Workloads in the host fixture read the flag and try to invoke the
 *     ability. A pre-#426 bench runner would never run this `plugins_loaded`
 *     callback because the file was required AFTER `plugins_loaded` already
 *     fired.
 */

add_action('plugins_loaded', static function (): void {
    $GLOBALS['homeboy_bench_426_dep_plugins_loaded_fired'] = true;
});

add_action('wp_abilities_api_categories_init', static function (): void {
    if (!function_exists('wp_register_ability_category')) {
        return;
    }
    if (function_exists('wp_get_ability_category') && wp_get_ability_category('bench-plugins-loaded-dep')) {
        return;
    }
    wp_register_ability_category('bench-plugins-loaded-dep', [
        'label' => 'Bench Plugins-Loaded Dep',
        'description' => 'Category for the homeboy-extensions#426 dep regression fixture.',
    ]);
});

add_action('wp_abilities_api_init', static function (): void {
    if (!function_exists('wp_register_ability')) {
        return;
    }
    if (function_exists('wp_get_ability') && wp_get_ability('bench-plugins-loaded-dep/ping')) {
        return;
    }
    wp_register_ability('bench-plugins-loaded-dep/ping', [
        'label' => 'Bench dep ping',
        'description' => 'Ability registered by a validation-dependency fixture for homeboy-extensions#426.',
        'category' => 'bench-plugins-loaded-dep',
        'permission_callback' => static fn (): bool => true,
        'execute_callback' => static fn (array $input = []) => ['metadata' => ['ok' => true]],
    ]);
});
