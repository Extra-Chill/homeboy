<?php
/**
 * Generic WordPress block theme quality probe for Playground workloads.
 *
 * Scenario graders can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/block-theme-quality-probe.php
 */

if (!function_exists('homeboy_wordpress_count_blocks_for_quality_probe')) {
    /**
     * Count parsed blocks recursively into the quality probe accumulator.
     *
     * @param array<int,array<string,mixed>> $blocks Parsed block tree.
     * @param array<string,int>             $counts Probe counters.
     */
    function homeboy_wordpress_count_blocks_for_quality_probe(array $blocks, array &$counts): void {
        foreach ($blocks as $block) {
            $name = isset($block['blockName']) ? (string) $block['blockName'] : '';
            if ($name !== '') {
                $counts['total_blocks']++;
                if ($name === 'core/html') {
                    $counts['core_html_blocks']++;
                }
                if ($name === 'core/navigation') {
                    $counts['navigation_blocks']++;
                }
                if ($name === 'core/template-part') {
                    $counts['template_part_blocks']++;
                }
            }

            if (!empty($block['innerBlocks']) && is_array($block['innerBlocks'])) {
                homeboy_wordpress_count_blocks_for_quality_probe($block['innerBlocks'], $counts);
            }
        }
    }
}

if (!function_exists('homeboy_wordpress_quality_probe_scalar_list')) {
    /**
     * Normalize a scalar/list option into unique strings.
     *
     * @param mixed $value Input value.
     * @return array<int,string>
     */
    function homeboy_wordpress_quality_probe_scalar_list($value): array {
        if ($value === null || $value === '') {
            return [];
        }
        $items = is_array($value) ? $value : [$value];
        $normalized = [];
        foreach ($items as $item) {
            if (!is_scalar($item)) {
                continue;
            }
            $item = trim((string) $item);
            if ($item !== '' && !in_array($item, $normalized, true)) {
                $normalized[] = $item;
            }
        }

        return $normalized;
    }
}

if (!function_exists('homeboy_wordpress_collect_block_theme_quality')) {
    /**
     * Collect generic WordPress site/block/theme quality metrics.
     *
     * Supported options:
     * - target_post_ids: IDs to treat as scenario target pages/posts.
     * - target_post_titles: titles to treat as scenario target pages/posts.
     * - post_types: post types to scan. Defaults to pages, templates, template parts,
     *   and navigation posts.
     *
     * @param array<string,mixed> $options Probe options.
     * @return array<string,mixed> Structured quality metrics.
     */
    function homeboy_wordpress_collect_block_theme_quality(array $options = []): array {
        $front_page_id = (int) get_option('page_on_front', 0);
        $target_post_ids = array_map('intval', homeboy_wordpress_quality_probe_scalar_list($options['target_post_ids'] ?? []));
        if ($front_page_id > 0 && !in_array($front_page_id, $target_post_ids, true)) {
            $target_post_ids[] = $front_page_id;
        }
        $target_post_titles = homeboy_wordpress_quality_probe_scalar_list($options['target_post_titles'] ?? []);
        $post_types = homeboy_wordpress_quality_probe_scalar_list($options['post_types'] ?? ['page', 'wp_template', 'wp_template_part', 'wp_navigation']);

        $theme_json_present = false;
        foreach (array_unique([get_stylesheet_directory(), get_template_directory()]) as $theme_dir) {
            if (is_string($theme_dir) && $theme_dir !== '' && is_readable($theme_dir . '/theme.json')) {
                $theme_json_present = true;
                break;
            }
        }

        $counts = [
            'front_page_id' => $front_page_id,
            'posts_seen' => 0,
            'pages_seen' => 0,
            'templates_seen' => 0,
            'template_parts_seen' => 0,
            'navigation_posts_seen' => 0,
            'posts_with_blocks' => 0,
            'total_blocks' => 0,
            'core_html_blocks' => 0,
            'serialized_block_comments' => 0,
            'template_part_blocks' => 0,
            'navigation_blocks' => 0,
            'raw_html_unconverted' => 0,
            'target_posts_seen' => 0,
            'target_pages_seen' => 0,
            'target_posts_with_blocks' => 0,
            'target_total_blocks' => 0,
            'target_core_html_blocks' => 0,
            'target_serialized_block_comments' => 0,
            'target_raw_html_unconverted' => 0,
        ];

        $posts = get_posts([
            'post_type' => $post_types,
            'post_status' => 'any',
            'numberposts' => -1,
        ]);

        foreach ($posts as $post) {
            $content = (string) $post->post_content;
            $has_content = trim($content) !== '';
            $has_blocks = strpos($content, '<!-- wp:') !== false;
            $has_raw_html_without_blocks = !$has_blocks && $has_content && preg_match('/<\/?[a-z][a-z0-9:-]*(?:\s|>)/i', $content) === 1;

            if ($has_content) {
                $counts['posts_seen']++;
            }
            if ($post->post_type === 'page') {
                $counts['pages_seen']++;
            } elseif ($post->post_type === 'wp_template') {
                $counts['templates_seen']++;
            } elseif ($post->post_type === 'wp_template_part') {
                $counts['template_parts_seen']++;
            } elseif ($post->post_type === 'wp_navigation') {
                $counts['navigation_posts_seen']++;
            }

            $serialized_block_comments = substr_count($content, '<!-- wp:');
            $counts['serialized_block_comments'] += $serialized_block_comments;
            if ($has_blocks) {
                $counts['posts_with_blocks']++;
            }
            if ($has_raw_html_without_blocks) {
                $counts['raw_html_unconverted']++;
            }

            $before_total = $counts['total_blocks'];
            $before_core_html = $counts['core_html_blocks'];
            if ($has_content && function_exists('parse_blocks')) {
                homeboy_wordpress_count_blocks_for_quality_probe(parse_blocks($content), $counts);
            }

            $is_target = in_array((int) $post->ID, $target_post_ids, true)
                || in_array((string) $post->post_title, $target_post_titles, true);
            if (!$is_target) {
                continue;
            }

            $counts['target_posts_seen']++;
            if ($post->post_type === 'page') {
                $counts['target_pages_seen']++;
            }
            if ($has_blocks) {
                $counts['target_posts_with_blocks']++;
            }
            if ($has_raw_html_without_blocks) {
                $counts['target_raw_html_unconverted']++;
            }
            $counts['target_serialized_block_comments'] += $serialized_block_comments;
            $counts['target_total_blocks'] += $counts['total_blocks'] - $before_total;
            $counts['target_core_html_blocks'] += $counts['core_html_blocks'] - $before_core_html;
        }

        $nav_menu_count = 0;
        if (function_exists('wp_get_nav_menus')) {
            $nav_menus = wp_get_nav_menus();
            $nav_menu_count = is_array($nav_menus) ? count($nav_menus) : 0;
        }

        return array_merge([
            'used_block_theme' => function_exists('wp_is_block_theme') ? (bool) wp_is_block_theme() : false,
            'theme_json_present' => $theme_json_present,
        ], $counts, [
            'nav_menus_seen' => $nav_menu_count,
            'navigation_created' => $counts['navigation_blocks'] > 0 || $counts['navigation_posts_seen'] > 0 || $nav_menu_count > 0,
        ]);
    }
}

if (!function_exists('homeboy_wordpress_block_theme_quality_payload')) {
    /**
     * Return a Playground workload payload with numeric metrics and metadata.
     *
     * @param array<string,mixed> $options Probe options.
     * @return array<string,array<string,mixed>> Workload payload.
     */
    function homeboy_wordpress_block_theme_quality_payload(array $options = []): array {
        $quality = homeboy_wordpress_collect_block_theme_quality($options);
        $metrics = [];
        foreach ($quality as $key => $value) {
            if (is_bool($value)) {
                $metrics[$key] = $value ? 1 : 0;
            } elseif (is_int($value) || is_float($value)) {
                $metrics[$key] = $value;
            }
        }

        return [
            'metrics' => $metrics,
            'metadata' => [
                'wordpress_quality' => $quality,
            ],
        ];
    }
}
