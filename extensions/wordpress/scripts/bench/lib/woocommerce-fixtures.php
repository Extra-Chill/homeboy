<?php
/**
 * Reusable WooCommerce fixture profiles for WordPress bench workloads.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/woocommerce-fixtures.php
 */

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_profile_defaults' ) ) {
	/**
	 * Return deterministic defaults for a named WooCommerce bench fixture profile.
	 *
	 * @param string $profile Profile id.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_wc_fixture_profile_defaults( string $profile ): array {
		$profiles = array(
			'small-shortcode-checkout' => array(
				'profile'                    => 'small-shortcode-checkout',
				'hpos'                       => false,
				'checkout'                   => 'shortcode',
				'enable_cod'                 => true,
				'enable_woo_admin'           => false,
				'product_count'              => 150,
				'variable_product_count'     => 25,
				'variations_per_product'     => 5,
				'category_count'             => 8,
				'customer_count'             => 12,
				'orders_per_customer'        => 2,
				'guest_order_count'          => 16,
				'coupon_count'               => 2,
				'shipping_zone_count'        => 1,
				'shipping_methods_per_zone'  => array( 'flat_rate' ),
				'physical_product_ratio'     => 1.0,
			),
			'large-admin-catalog' => array(
				'profile'                    => 'large-admin-catalog',
				'hpos'                       => null,
				'checkout'                   => 'block',
				'enable_cod'                 => true,
				'enable_woo_admin'           => true,
				'product_count'              => 1000,
				'variable_product_count'     => 120,
				'variations_per_product'     => 4,
				'category_count'             => 80,
				'customer_count'             => 20,
				'orders_per_customer'        => 1,
				'guest_order_count'          => 40,
				'coupon_count'               => 6,
				'shipping_zone_count'        => 2,
				'shipping_methods_per_zone'  => array( 'flat_rate', 'free_shipping' ),
				'physical_product_ratio'     => 0.65,
			),
			'account-heavy-store' => array(
				'profile'                    => 'account-heavy-store',
				'hpos'                       => null,
				'checkout'                   => 'block',
				'enable_cod'                 => true,
				'enable_woo_admin'           => false,
				'product_count'              => 120,
				'variable_product_count'     => 12,
				'variations_per_product'     => 3,
				'category_count'             => 12,
				'customer_count'             => 120,
				'orders_per_customer'        => 5,
				'guest_order_count'          => 0,
				'coupon_count'               => 3,
				'shipping_zone_count'        => 1,
				'shipping_methods_per_zone'  => array( 'flat_rate' ),
				'physical_product_ratio'     => 0.8,
			),
			'shipping-package-matrix' => array(
				'profile'                    => 'shipping-package-matrix',
				'hpos'                       => null,
				'checkout'                   => 'shortcode',
				'enable_cod'                 => true,
				'enable_woo_admin'           => false,
				'product_count'              => 180,
				'variable_product_count'     => 20,
				'variations_per_product'     => 3,
				'category_count'             => 10,
				'customer_count'             => 10,
				'orders_per_customer'        => 1,
				'guest_order_count'          => 8,
				'coupon_count'               => 1,
				'shipping_zone_count'        => 4,
				'shipping_methods_per_zone'  => array( 'flat_rate', 'free_shipping', 'local_pickup' ),
				'physical_product_ratio'     => 1.0,
				'package_count'              => 6,
				'items_per_package'          => 8,
			),
		);

		return $profiles[ $profile ] ?? $profiles['small-shortcode-checkout'];
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_apply_fixture_profile' ) ) {
	/**
	 * Apply a WooCommerce bench fixture profile and return a workload payload.
	 *
	 * @param string              $profile Profile id.
	 * @param array<string,mixed> $overrides Profile overrides.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_wc_apply_fixture_profile( string $profile, array $overrides = array() ): array {
		$config = homeboy_wordpress_bench_wc_fixture_merge(
			homeboy_wordpress_bench_wc_fixture_profile_defaults( $profile ),
			$overrides
		);
		$config['profile'] = (string) ( $config['profile'] ?? $profile );
		$run_id            = homeboy_wordpress_bench_wc_fixture_run_id( $config['run_id'] ?? null );
		$prefix            = homeboy_wordpress_bench_wc_fixture_prefix( $run_id );

		if ( ! homeboy_wordpress_bench_wc_fixture_available() ) {
			return homeboy_wordpress_bench_wc_fixture_payload(
				$config,
				$run_id,
				array(
					'woocommerce_available' => false,
					'created'               => array(),
					'failures'              => array(
						array(
							'code'    => 'woocommerce_unavailable',
							'message' => 'WooCommerce fixture profile requires WooCommerce classes and helpers to be loaded.',
						),
					),
				)
			);
		}

		$created = array(
			'categories' => array(),
			'products'   => array(),
			'variations' => array(),
			'customers'  => array(),
			'orders'     => array(),
			'coupons'    => array(),
			'shipping'   => array(),
			'pages'      => array(),
			'settings'   => array(),
		);

		homeboy_wordpress_bench_wc_fixture_configure_settings( $config, $created );
		$created['categories'] = homeboy_wordpress_bench_wc_fixture_create_categories( $config, $prefix );
		$created['products']   = homeboy_wordpress_bench_wc_fixture_create_products( $config, $prefix, $created['categories'] );
		$created['variations'] = homeboy_wordpress_bench_wc_fixture_create_variations( $config, $prefix, $created['products'] );
		$created['customers']  = homeboy_wordpress_bench_wc_fixture_create_customers( $config, $prefix );
		$created['coupons']    = homeboy_wordpress_bench_wc_fixture_create_coupons( $config, $prefix );
		$created['shipping']   = homeboy_wordpress_bench_wc_fixture_create_shipping( $config, $prefix );
		$created['orders']     = homeboy_wordpress_bench_wc_fixture_create_orders( $config, $prefix, $created['customers'], $created['products'] );

		return homeboy_wordpress_bench_wc_fixture_payload(
			$config,
			$run_id,
			array(
				'woocommerce_available' => true,
				'created'               => $created,
				'failures'              => array(),
			)
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_payload' ) ) {
	/**
	 * Format fixture metadata and numeric metrics for BenchResults.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @param string              $run_id Run id.
	 * @param array<string,mixed> $state Fixture state.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_wc_fixture_payload( array $config, string $run_id, array $state ): array {
		$created  = is_array( $state['created'] ?? null ) ? $state['created'] : array();
		$failures = is_array( $state['failures'] ?? null ) ? $state['failures'] : array();

		$metrics = array(
			'woocommerce_available' => ! empty( $state['woocommerce_available'] ) ? 1 : 0,
			'fixture_failures'      => count( $failures ),
		);

		foreach ( array( 'categories', 'products', 'variations', 'customers', 'orders', 'coupons', 'shipping', 'pages' ) as $key ) {
			$metrics[ 'fixture_' . $key ] = isset( $created[ $key ] ) && is_array( $created[ $key ] ) ? count( $created[ $key ] ) : 0;
		}

		return array(
			'metrics'  => $metrics,
			'metadata' => array(
				'woocommerce_fixture' => array(
					'schema'    => 'homeboy/wordpress-bench-woocommerce-fixture/v1',
					'profile'   => (string) ( $config['profile'] ?? '' ),
					'run_id'    => $run_id,
					'prefix'    => homeboy_wordpress_bench_wc_fixture_prefix( $run_id ),
					'config'    => homeboy_wordpress_bench_wc_fixture_public_config( $config ),
					'created'   => homeboy_wordpress_bench_wc_fixture_public_created( $created ),
					'failures'  => $failures,
				),
			),
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_available' ) ) {
	/**
	 * Check whether the WooCommerce fixture can create real store objects.
	 *
	 * @return bool
	 */
	function homeboy_wordpress_bench_wc_fixture_available(): bool {
		return class_exists( 'WC_Product_Simple' )
			&& function_exists( 'wc_get_product_id_by_sku' )
			&& function_exists( 'wc_create_order' );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_run_id' ) ) {
	/**
	 * Normalize a run id for deterministic slugs/SKUs/emails.
	 *
	 * @param mixed $run_id Explicit run id.
	 * @return string
	 */
	function homeboy_wordpress_bench_wc_fixture_run_id( $run_id = null ): string {
		if ( null === $run_id || '' === (string) $run_id ) {
			$run_id = getenv( 'HOMEBOY_RUN_ID' ) ?: getenv( 'GITHUB_RUN_ID' ) ?: 'local';
		}

		$run_id = strtolower( preg_replace( '/[^a-zA-Z0-9_-]+/', '-', (string) $run_id ) ?? '' );
		$run_id = trim( $run_id, '-' );

		return '' === $run_id ? 'local' : $run_id;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_prefix' ) ) {
	/**
	 * Return a short deterministic object prefix for a run id.
	 *
	 * @param string $run_id Run id.
	 * @return string
	 */
	function homeboy_wordpress_bench_wc_fixture_prefix( string $run_id ): string {
		return 'hb-' . substr( md5( $run_id ), 0, 10 );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_merge' ) ) {
	/**
	 * Recursively merge profile defaults with workload overrides.
	 *
	 * @param array<string,mixed> $defaults Default config.
	 * @param array<string,mixed> $overrides Override config.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_wc_fixture_merge( array $defaults, array $overrides ): array {
		foreach ( $overrides as $key => $value ) {
			if ( is_array( $value ) && isset( $defaults[ $key ] ) && is_array( $defaults[ $key ] ) && homeboy_wordpress_bench_wc_fixture_is_assoc( $value ) ) {
				$defaults[ $key ] = homeboy_wordpress_bench_wc_fixture_merge( $defaults[ $key ], $value );
				continue;
			}

			$defaults[ $key ] = $value;
		}

		return $defaults;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_is_assoc' ) ) {
	/**
	 * Determine whether an array is associative.
	 *
	 * @param array<mixed> $value Array value.
	 * @return bool
	 */
	function homeboy_wordpress_bench_wc_fixture_is_assoc( array $value ): bool {
		return array_keys( $value ) !== range( 0, count( $value ) - 1 );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_configure_settings' ) ) {
	/**
	 * Configure WooCommerce options commonly needed by bench profiles.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @param array<string,mixed> $created Created object metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_configure_settings( array $config, array &$created ): void {
		if ( function_exists( 'update_option' ) ) {
			if ( array_key_exists( 'hpos', $config ) && null !== $config['hpos'] ) {
				update_option( 'woocommerce_custom_orders_table_enabled', ! empty( $config['hpos'] ) ? 'yes' : 'no' );
				$created['settings']['hpos'] = ! empty( $config['hpos'] );
			}

			update_option( 'woocommerce_enable_guest_checkout', 'yes' );
			update_option( 'woocommerce_registration_generate_username', 'yes' );
			update_option( 'woocommerce_registration_generate_password', 'yes' );

			if ( ! empty( $config['enable_woo_admin'] ) ) {
				update_option( 'woocommerce_onboarding_profile', array( 'completed' => true ) );
				update_option( 'woocommerce_task_list_complete', 'yes' );
			}
		}

		if ( ! empty( $config['enable_cod'] ) && function_exists( 'update_option' ) ) {
			update_option(
				'woocommerce_cod_settings',
				array(
					'enabled'      => 'yes',
					'title'        => 'Cash on delivery',
					'description'  => 'Pay with cash upon delivery.',
					'instructions' => 'Pay with cash upon delivery.',
				)
			);
			$created['settings']['cod'] = true;
		}

		$page_id = homeboy_wordpress_bench_wc_fixture_create_checkout_page( (string) ( $config['checkout'] ?? 'block' ) );
		if ( $page_id > 0 ) {
			$created['pages'][] = array(
				'id'   => $page_id,
				'role' => 'checkout',
				'mode' => (string) ( $config['checkout'] ?? 'block' ),
			);
		}
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_checkout_page' ) ) {
	/**
	 * Create or update the WooCommerce checkout page.
	 *
	 * @param string $mode Checkout mode: shortcode or block.
	 * @return int Page id.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_checkout_page( string $mode ): int {
		if ( ! function_exists( 'wp_insert_post' ) || ! function_exists( 'update_option' ) ) {
			return 0;
		}

		$content = 'shortcode' === $mode ? '[woocommerce_checkout]' : '<!-- wp:woocommerce/checkout /-->';
		$page_id = function_exists( 'wc_get_page_id' ) ? (int) wc_get_page_id( 'checkout' ) : 0;
		if ( $page_id <= 0 && function_exists( 'get_page_by_path' ) ) {
			$page = get_page_by_path( 'checkout' );
			$page_id = $page ? (int) $page->ID : 0;
		}

		$post = array(
			'post_title'   => 'Checkout',
			'post_name'    => 'checkout',
			'post_type'    => 'page',
			'post_status'  => 'publish',
			'post_content' => $content,
		);

		if ( $page_id > 0 ) {
			$post['ID'] = $page_id;
		}

		$page_id = (int) wp_insert_post( $post );
		if ( $page_id > 0 ) {
			update_option( 'woocommerce_checkout_page_id', $page_id );
		}

		return $page_id;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_categories' ) ) {
	/**
	 * Create deterministic product categories.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @param string              $prefix Object prefix.
	 * @return array<int,array<string,mixed>> Category metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_categories( array $config, string $prefix ): array {
		if ( ! function_exists( 'term_exists' ) || ! function_exists( 'wp_insert_term' ) ) {
			return array();
		}

		$categories = array();
		$count      = max( 0, (int) ( $config['category_count'] ?? 0 ) );
		for ( $i = 1; $i <= $count; $i++ ) {
			$slug = $prefix . '-cat-' . $i;
			$term = term_exists( $slug, 'product_cat' );
			if ( ! $term ) {
				$term = wp_insert_term( 'Bench Category ' . $i . ' ' . $prefix, 'product_cat', array( 'slug' => $slug ) );
			}

			if ( is_wp_error( $term ) ) {
				continue;
			}

			$term_id      = is_array( $term ) ? (int) $term['term_id'] : (int) $term;
			$categories[] = array(
				'id'   => $term_id,
				'slug' => $slug,
			);
		}

		return $categories;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_products' ) ) {
	/**
	 * Create deterministic simple and variable products.
	 *
	 * @param array<string,mixed>              $config Profile config.
	 * @param string                           $prefix Object prefix.
	 * @param array<int,array<string,mixed>>   $categories Category metadata.
	 * @return array<int,array<string,mixed>> Product metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_products( array $config, string $prefix, array $categories ): array {
		$products       = array();
		$product_count  = max( 0, (int) ( $config['product_count'] ?? 0 ) );
		$variable_count = min( $product_count, max( 0, (int) ( $config['variable_product_count'] ?? 0 ) ) );
		$physical_ratio = max( 0, min( 1, (float) ( $config['physical_product_ratio'] ?? 1 ) ) );
		$category_ids   = array_values( array_filter( array_map( 'intval', array_column( $categories, 'id' ) ) ) );

		for ( $i = 1; $i <= $product_count; $i++ ) {
			$is_variable = $i <= $variable_count;
			$sku         = strtoupper( $prefix ) . '-P-' . $i;
			$product_id  = (int) wc_get_product_id_by_sku( $sku );
			$product     = $product_id > 0 ? wc_get_product( $product_id ) : null;

			if ( ! $product ) {
				$product = $is_variable && class_exists( 'WC_Product_Variable' ) ? new WC_Product_Variable() : new WC_Product_Simple();
			}

			$product->set_name( 'Bench Product ' . $i . ' ' . $prefix );
			$product->set_slug( $prefix . '-product-' . $i );
			$product->set_sku( $sku );
			$product->set_status( 'publish' );
			$product->set_catalog_visibility( 'visible' );
			$product->set_regular_price( (string) ( 10 + ( $i % 90 ) ) );
			$product->set_description( 'Deterministic Homeboy WooCommerce bench fixture product.' );
			$product->set_short_description( 'Bench fixture product.' );
			$product->set_virtual( ( $i / max( 1, $product_count ) ) > $physical_ratio );
			$product->set_manage_stock( true );
			$product->set_stock_quantity( 500 );
			$product->set_stock_status( 'instock' );

			if ( ! empty( $category_ids ) ) {
				$product->set_category_ids( array( $category_ids[ ( $i - 1 ) % count( $category_ids ) ] ) );
			}

			if ( $is_variable && method_exists( $product, 'set_attributes' ) ) {
				$product->set_attributes( homeboy_wordpress_bench_wc_fixture_variation_attributes() );
			}

			$product_id  = (int) $product->save();
			$products[] = array(
				'id'       => $product_id,
				'index'    => $i,
				'sku'      => $sku,
				'type'     => $is_variable ? 'variable' : 'simple',
				'virtual'  => (bool) $product->get_virtual(),
				'price'    => (float) $product->get_regular_price(),
			);
		}

		return $products;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_variation_attributes' ) ) {
	/**
	 * Build a simple variable-product attribute set.
	 *
	 * @return array<int,WC_Product_Attribute>
	 */
	function homeboy_wordpress_bench_wc_fixture_variation_attributes(): array {
		if ( ! class_exists( 'WC_Product_Attribute' ) ) {
			return array();
		}

		$attribute = new WC_Product_Attribute();
		$attribute->set_name( 'Bench Size' );
		$attribute->set_options( array( 'XS', 'S', 'M', 'L', 'XL' ) );
		$attribute->set_visible( true );
		$attribute->set_variation( true );

		return array( $attribute );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_variations' ) ) {
	/**
	 * Create deterministic variations for variable products.
	 *
	 * @param array<string,mixed>            $config Profile config.
	 * @param string                         $prefix Object prefix.
	 * @param array<int,array<string,mixed>> $products Product metadata.
	 * @return array<int,array<string,mixed>> Variation metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_variations( array $config, string $prefix, array $products ): array {
		if ( ! class_exists( 'WC_Product_Variation' ) ) {
			return array();
		}

		$variations = array();
		$per_parent = max( 0, (int) ( $config['variations_per_product'] ?? 0 ) );
		$sizes      = array( 'XS', 'S', 'M', 'L', 'XL', 'XXL' );

		foreach ( $products as $product ) {
			if ( 'variable' !== ( $product['type'] ?? '' ) ) {
				continue;
			}

			$parent_id = (int) $product['id'];
			for ( $i = 1; $i <= $per_parent; $i++ ) {
				$sku          = strtoupper( $prefix ) . '-V-' . (int) $product['index'] . '-' . $i;
				$variation_id = (int) wc_get_product_id_by_sku( $sku );
				$variation    = $variation_id > 0 ? wc_get_product( $variation_id ) : new WC_Product_Variation();
				$variation->set_parent_id( $parent_id );
				$variation->set_sku( $sku );
				$variation->set_regular_price( (string) ( (float) $product['price'] + $i ) );
				$variation->set_attributes( array( 'bench-size' => $sizes[ ( $i - 1 ) % count( $sizes ) ] ) );
				$variation->set_manage_stock( true );
				$variation->set_stock_quantity( 200 );
				$variation->set_stock_status( 'instock' );
				$variation_id  = (int) $variation->save();
				$variations[] = array(
					'id'        => $variation_id,
					'parent_id' => $parent_id,
					'sku'       => $sku,
				);
			}
		}

		return $variations;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_customers' ) ) {
	/**
	 * Create deterministic customers.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @param string              $prefix Object prefix.
	 * @return array<int,array<string,mixed>> Customer metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_customers( array $config, string $prefix ): array {
		$customers = array();
		$count     = max( 0, (int) ( $config['customer_count'] ?? 0 ) );

		for ( $i = 1; $i <= $count; $i++ ) {
			$email   = $prefix . '-customer-' . $i . '@example.test';
			$user_id = function_exists( 'email_exists' ) ? (int) email_exists( $email ) : 0;
			if ( $user_id <= 0 && function_exists( 'wp_insert_user' ) ) {
				$user_id = (int) wp_insert_user(
					array(
						'user_login' => $prefix . '-customer-' . $i,
						'user_email' => $email,
						'user_pass'  => wp_generate_password( 24, true ),
						'role'       => 'customer',
						'first_name' => 'Bench',
						'last_name'  => 'Customer ' . $i,
					)
				);
			}

			if ( $user_id > 0 ) {
				$customers[] = array(
					'id'    => $user_id,
					'email' => $email,
				);
			}
		}

		return $customers;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_coupons' ) ) {
	/**
	 * Create deterministic coupons.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @param string              $prefix Object prefix.
	 * @return array<int,array<string,mixed>> Coupon metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_coupons( array $config, string $prefix ): array {
		if ( ! class_exists( 'WC_Coupon' ) ) {
			return array();
		}

		$coupons = array();
		$count   = max( 0, (int) ( $config['coupon_count'] ?? 0 ) );
		for ( $i = 1; $i <= $count; $i++ ) {
			$code   = $prefix . '-coupon-' . $i;
			$coupon = new WC_Coupon( $code );
			$coupon->set_code( $code );
			$coupon->set_discount_type( 'percent' );
			$coupon->set_amount( 5 + $i );
			$coupon->set_usage_limit( 100000 );
			$coupon_id = (int) $coupon->save();
			$coupons[] = array(
				'id'   => $coupon_id,
				'code' => $code,
			);
		}

		return $coupons;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_shipping' ) ) {
	/**
	 * Create deterministic shipping zones and methods.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @param string              $prefix Object prefix.
	 * @return array<int,array<string,mixed>> Shipping metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_shipping( array $config, string $prefix ): array {
		if ( ! class_exists( 'WC_Shipping_Zone' ) ) {
			return array();
		}

		$shipping = array();
		$zones    = max( 0, (int) ( $config['shipping_zone_count'] ?? 0 ) );
		$methods  = is_array( $config['shipping_methods_per_zone'] ?? null ) ? $config['shipping_methods_per_zone'] : array( 'flat_rate' );

		for ( $i = 1; $i <= $zones; $i++ ) {
			$zone_name = 'Bench Zone ' . $i . ' ' . $prefix;
			$zone_id   = homeboy_wordpress_bench_wc_fixture_find_shipping_zone_id( $zone_name );
			$zone      = new WC_Shipping_Zone( $zone_id );
			$zone->set_zone_name( $zone_name );
			$zone->set_zone_order( $i );
			$zone->save();

			$method_ids = array();
			$existing_methods = method_exists( $zone, 'get_shipping_methods' ) ? $zone->get_shipping_methods( false ) : array();
			$existing_method_ids = array();
			foreach ( $existing_methods as $existing_method ) {
				if ( isset( $existing_method->id ) ) {
					$existing_method_ids[] = (string) $existing_method->id;
				}
			}
			foreach ( $methods as $method_id ) {
				if ( in_array( (string) $method_id, $existing_method_ids, true ) ) {
					$method_ids[] = array(
						'instance_id' => 0,
						'method_id'   => (string) $method_id,
						'existing'    => true,
					);
					continue;
				}

				$instance_id = $zone->add_shipping_method( (string) $method_id );
				if ( $instance_id ) {
					$method_ids[] = array(
						'instance_id' => (int) $instance_id,
						'method_id'   => (string) $method_id,
					);
				}
			}

			$shipping[] = array(
				'id'      => (int) $zone->get_id(),
				'name'    => $zone_name,
				'methods' => $method_ids,
			);
		}

		return $shipping;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_find_shipping_zone_id' ) ) {
	/**
	 * Find an existing shipping zone by name to keep fixture setup idempotent.
	 *
	 * @param string $zone_name Zone name.
	 * @return int Zone id, or 0 when missing.
	 */
	function homeboy_wordpress_bench_wc_fixture_find_shipping_zone_id( string $zone_name ): int {
		if ( ! class_exists( 'WC_Shipping_Zones' ) || ! method_exists( 'WC_Shipping_Zones', 'get_zones' ) ) {
			return 0;
		}

		foreach ( WC_Shipping_Zones::get_zones() as $zone ) {
			$zone_id = $zone['zone_id'] ?? $zone['id'] ?? 0;
			if ( isset( $zone['zone_name'] ) && $zone_name === $zone['zone_name'] && $zone_id > 0 ) {
				return (int) $zone_id;
			}
		}

		return 0;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_orders' ) ) {
	/**
	 * Create deterministic historical orders.
	 *
	 * @param array<string,mixed>            $config Profile config.
	 * @param string                         $prefix Object prefix.
	 * @param array<int,array<string,mixed>> $customers Customer metadata.
	 * @param array<int,array<string,mixed>> $products Product metadata.
	 * @return array<int,array<string,mixed>> Order metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_orders( array $config, string $prefix, array $customers, array $products ): array {
		if ( empty( $products ) || ! function_exists( 'wc_get_product' ) ) {
			return array();
		}

		$orders              = array();
		$orders_per_customer = max( 0, (int) ( $config['orders_per_customer'] ?? 0 ) );
		$guest_order_count   = max( 0, (int) ( $config['guest_order_count'] ?? 0 ) );
		$order_index         = 0;

		foreach ( $customers as $customer ) {
			for ( $i = 1; $i <= $orders_per_customer; $i++ ) {
				++$order_index;
				$orders[] = homeboy_wordpress_bench_wc_fixture_create_order( $prefix, $order_index, (int) $customer['id'], $products );
			}
		}

		for ( $i = 1; $i <= $guest_order_count; $i++ ) {
			++$order_index;
			$orders[] = homeboy_wordpress_bench_wc_fixture_create_order( $prefix, $order_index, 0, $products );
		}

		return array_values( array_filter( $orders ) );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_create_order' ) ) {
	/**
	 * Create one deterministic order.
	 *
	 * @param string                         $prefix Object prefix.
	 * @param int                            $order_index Order index.
	 * @param int                            $customer_id Customer id, or 0 for guest.
	 * @param array<int,array<string,mixed>> $products Product metadata.
	 * @return array<string,mixed>|null Order metadata.
	 */
	function homeboy_wordpress_bench_wc_fixture_create_order( string $prefix, int $order_index, int $customer_id, array $products ): ?array {
		$fixture_key = $prefix . '-order-' . $order_index;
		$existing_id = homeboy_wordpress_bench_wc_fixture_find_order_id( $fixture_key );
		if ( $existing_id > 0 ) {
			return array(
				'id'          => $existing_id,
				'customer_id' => $customer_id,
				'line_count'  => 0,
				'total'       => 0.0,
				'existing'    => true,
			);
		}

		$order = wc_create_order( array( 'customer_id' => $customer_id ) );
		if ( is_wp_error( $order ) || ! $order ) {
			return null;
		}

		$line_count = 1 + ( $order_index % 3 );
		for ( $i = 0; $i < $line_count; $i++ ) {
			$product_meta = $products[ ( $order_index + $i ) % count( $products ) ];
			$product      = wc_get_product( (int) $product_meta['id'] );
			if ( $product ) {
				$order->add_product( $product, 1 + ( ( $order_index + $i ) % 2 ) );
			}
		}

		$order->set_payment_method( 'cod' );
		$order->set_billing_first_name( 'Bench' );
		$order->set_billing_last_name( 'Customer' );
		$order->set_billing_email( $customer_id > 0 ? $prefix . '-customer-order-' . $order_index . '@example.test' : $prefix . '-guest-' . $order_index . '@example.test' );
		$order->set_status( 'completed' );
		$order->update_meta_data( '_homeboy_wc_fixture_key', $fixture_key );
		$order->calculate_totals();
		$order_id = (int) $order->save();

		return array(
			'id'          => $order_id,
			'customer_id' => $customer_id,
			'line_count'  => $line_count,
			'total'       => (float) $order->get_total(),
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_find_order_id' ) ) {
	/**
	 * Find an existing fixture order by deterministic fixture key.
	 *
	 * @param string $fixture_key Fixture order key.
	 * @return int Order id, or 0 when missing.
	 */
	function homeboy_wordpress_bench_wc_fixture_find_order_id( string $fixture_key ): int {
		if ( ! function_exists( 'wc_get_orders' ) ) {
			return 0;
		}

		$orders = wc_get_orders(
			array(
				'limit'      => 1,
				'return'     => 'ids',
				'meta_key'   => '_homeboy_wc_fixture_key',
				'meta_value' => $fixture_key,
			)
		);

		return isset( $orders[0] ) ? (int) $orders[0] : 0;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_public_config' ) ) {
	/**
	 * Return stable public config metadata.
	 *
	 * @param array<string,mixed> $config Profile config.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_wc_fixture_public_config( array $config ): array {
		$public = $config;
		unset( $public['run_id'] );

		return $public;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_wc_fixture_public_created' ) ) {
	/**
	 * Trim created-object metadata to artifact-friendly fields.
	 *
	 * @param array<string,mixed> $created Created object metadata.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_wc_fixture_public_created( array $created ): array {
		$public = array();
		foreach ( $created as $key => $rows ) {
			$public[ $key ] = is_array( $rows ) ? $rows : array();
		}

		return $public;
	}
}
