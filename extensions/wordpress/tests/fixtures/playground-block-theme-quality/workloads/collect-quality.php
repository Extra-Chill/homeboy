<?php

require_once '/homeboy-extension/scripts/bench/lib/block-theme-quality-probe.php';

$front_page_id = wp_insert_post([
    'post_type' => 'page',
    'post_status' => 'publish',
    'post_title' => 'Quality Probe Front Page',
    'post_content' => '<!-- wp:navigation /--><!-- wp:paragraph --><p>Welcome to the bakery.</p><!-- /wp:paragraph --><!-- wp:html --><section>Hours</section><!-- /wp:html -->',
], true);

if (is_wp_error($front_page_id)) {
    throw new RuntimeException($front_page_id->get_error_message());
}

$raw_page_id = wp_insert_post([
    'post_type' => 'page',
    'post_status' => 'publish',
    'post_title' => 'Raw HTML Page',
    'post_content' => '<main><h1>Plain HTML</h1></main>',
], true);

if (is_wp_error($raw_page_id)) {
    throw new RuntimeException($raw_page_id->get_error_message());
}

// Bypass editor/content filters so the smoke has a stable raw-HTML fixture.
global $wpdb;
$wpdb->update($wpdb->posts, ['post_content' => '<main><h1>Plain HTML</h1></main>'], ['ID' => (int) $raw_page_id]);
clean_post_cache((int) $raw_page_id);

foreach ([
    'wp_template' => '<!-- wp:template-part {"slug":"header"} /--><!-- wp:post-content /-->',
    'wp_template_part' => '<!-- wp:group --><div class="wp-block-group"><!-- wp:paragraph --><p>Header</p><!-- /wp:paragraph --></div><!-- /wp:group -->',
] as $post_type => $content) {
    $post_id = wp_insert_post([
        'post_type' => $post_type,
        'post_status' => 'publish',
        'post_title' => 'Quality Probe ' . $post_type,
        'post_name' => 'quality-probe-' . $post_type,
        'post_content' => $content,
    ], true);

    if (is_wp_error($post_id)) {
        throw new RuntimeException($post_id->get_error_message());
    }
}

if (post_type_exists('wp_navigation')) {
    $navigation_id = wp_insert_post([
        'post_type' => 'wp_navigation',
        'post_status' => 'publish',
        'post_title' => 'Quality Probe Navigation',
        'post_content' => '<!-- wp:navigation-link {"label":"Home","url":"/"} /-->',
    ], true);

    if (is_wp_error($navigation_id)) {
        throw new RuntimeException($navigation_id->get_error_message());
    }
}

update_option('show_on_front', 'page');
update_option('page_on_front', (int) $front_page_id);

return homeboy_wordpress_block_theme_quality_payload([
    'target_post_ids' => [(int) $front_page_id],
]);
