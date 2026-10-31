<?php
/** Smoke test for bounded generic WordPress frontend URL surface discovery. */

if ( ! function_exists( 'home_url' ) ) {
	function home_url( string $path = '' ): string {
		return 'https://example.test' . ( str_starts_with( $path, '/' ) ? $path : '/' . $path );
	}
}

if ( ! function_exists( 'get_option' ) ) {
	function get_option( string $name, $default = false ) {
		$options = array(
			'show_on_front' => 'page',
			'page_on_front' => 42,
		);

		return $options[ $name ] ?? $default;
	}
}

if ( ! function_exists( 'get_permalink' ) ) {
	function get_permalink( int $post_id ): string {
		$paths = array(
			42 => '/front-page/',
			7  => '/hello-world/',
			8  => '/about/',
			9  => '/news/update/',
		);

		return home_url( $paths[ $post_id ] ?? '/?p=' . $post_id );
	}
}

if ( ! function_exists( 'get_post_types' ) ) {
	function get_post_types( array $args = array(), string $output = 'names' ): array {
		unset( $args, $output );
		return array( 'post', 'page', 'book' );
	}
}

if ( ! function_exists( 'get_post_type_archive_link' ) ) {
	function get_post_type_archive_link( string $post_type ) {
		$links = array(
			'post' => home_url( '/blog/' ),
			'book' => home_url( '/books/' ),
		);

		return $links[ $post_type ] ?? false;
	}
}

if ( ! function_exists( 'get_taxonomies' ) ) {
	function get_taxonomies( array $args = array(), string $output = 'names' ): array {
		unset( $args, $output );
		return array( 'category' );
	}
}

if ( ! function_exists( 'get_terms' ) ) {
	function get_terms( array $args = array() ): array {
		unset( $args );
		return array(
			(object) array(
				'term_id'  => 3,
				'name'     => 'News',
				'taxonomy' => 'category',
			),
		);
	}
}

if ( ! function_exists( 'get_term_link' ) ) {
	function get_term_link( $term ): string {
		return home_url( '/category/' . strtolower( (string) $term->name ) . '/' );
	}
}

if ( ! function_exists( 'get_posts' ) ) {
	function get_posts( array $args = array() ): array {
		unset( $args );
		return array(
			(object) array(
				'ID'         => 7,
				'post_type'  => 'post',
				'post_title' => 'Hello world',
			),
			(object) array(
				'ID'         => 8,
				'post_type'  => 'page',
				'post_title' => 'About',
			),
			(object) array(
				'ID'         => 9,
				'post_type'  => 'post',
				'post_title' => 'Update',
			),
		);
	}
}

require_once __DIR__ . '/../scripts/bench/lib/wordpress-fuzz-coverage.php';

$surfaces = homeboy_wordpress_bench_frontend_url_surfaces(
	array(
		'max_total'       => 20,
		'max_per_surface' => 2,
	)
);

$assert = static function ( bool $condition, string $message ) use ( $surfaces ): void {
	if ( $condition ) {
		return;
	}
	fwrite( STDERR, "ERROR: {$message}\n" . json_encode( $surfaces, JSON_PRETTY_PRINT ) . "\n" ); // phpcs:ignore WordPress.WP.AlternativeFunctions.json_encode_json_encode -- WordPress is stubbed in this smoke.
	exit( 1 );
};

$by_surface = array();
foreach ( $surfaces['candidates'] as $candidate ) {
	$by_surface[ $candidate['surface'] ][] = $candidate;
	$assert( preg_match( '#^https://example\.test/#', $candidate['url'] ) === 1, 'candidate URL should be absolute HTTP(S)' );
	$assert( ! isset( $candidate['local_path'] ), 'candidate schema must not expose local paths' );
}

$assert( 'homeboy/wordpress-frontend-url-surfaces/v1' === $surfaces['schema'], 'schema mismatch' );
$assert( 2 === $surfaces['limits']['max_per_surface'], 'max_per_surface limit mismatch' );
$assert( count( $surfaces['candidates'] ) <= 20, 'max_total exceeded' );
$assert( 1 === count( $by_surface['home'] ?? array() ), 'home candidate missing' );
$assert( 1 === count( $by_surface['front'] ?? array() ), 'front candidate missing' );
$assert( 2 === count( $by_surface['archive'] ?? array() ), 'archive candidates should be bounded' );
$assert( 2 === count( $by_surface['sitemap'] ?? array() ), 'sitemap candidates should be bounded' );
$assert( 2 === count( $by_surface['permalink'] ?? array() ), 'permalink candidates should be bounded' );
$assert( isset( $surfaces['skipped']['archive:max_per_surface'] ), 'archive skipped count should record truncation' );
$assert( isset( $surfaces['skipped']['permalink:max_per_surface'] ), 'permalink skipped count should record truncation' );
$assert( 42 === (int) $by_surface['front'][0]['object_id'], 'front page object id missing' );

echo "wordpress frontend URL surfaces helper smoke passed\n";
