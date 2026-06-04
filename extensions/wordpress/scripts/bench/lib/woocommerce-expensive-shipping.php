<?php
/**
 * Deterministic expensive WooCommerce shipping method fixture for bench workloads.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/woocommerce-expensive-shipping.php
 */

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_defaults') ) {
	/**
	 * Return normalized default options for the expensive shipping fixture.
	 *
	 * @param array<string,mixed> $options Fixture options.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_defaults(array $options = array()): array {
		return array_merge(array(
			'method_id'       => 'homeboy_expensive_shipping',
			'method_title'    => 'Homeboy Expensive Shipping',
			'rate_label'      => 'Homeboy deterministic expensive shipping',
			'rate_cost'       => 19.99,
			'synthetic_rules' => 250,
			'cpu_iterations'  => 50,
			'db_queries'      => 0,
			'delay_ms'        => 0,
		), $options);
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_metrics_key') ) {
	/**
	 * Return the option key used for persistent per-runtime fixture metrics.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_metrics_key(): string {
		return 'homeboy_expensive_shipping_metrics';
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_empty_metrics') ) {
	/**
	 * Return empty metrics for the expensive shipping fixture.
	 *
	 * @return array<string,int|float|string>
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_empty_metrics(): array {
		return array(
			'calculate_calls'      => 0,
			'packages_seen'        => 0,
			'synthetic_rules'      => 0,
			'cpu_iterations'       => 0,
			'db_queries'           => 0,
			'delay_ms'             => 0,
			'elapsed_ms'           => 0.0,
			'deterministic_digest' => '0',
		);
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_reset_metrics') ) {
	/**
	 * Reset in-memory and option-backed fixture metrics.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_reset_metrics(): void {
		$metrics = homeboy_wordpress_woocommerce_expensive_shipping_empty_metrics();
		$GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_metrics'] = $metrics;

		if ( function_exists('update_option') ) {
			update_option(homeboy_wordpress_woocommerce_expensive_shipping_metrics_key(), $metrics, false);
		}
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_metrics') ) {
	/**
	 * Return current fixture metrics.
	 *
	 * @return array<string,int|float|string>
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_metrics(): array {
		$metrics = isset($GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_metrics']) && is_array($GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_metrics'])
			? $GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_metrics']
			: array();

		if ( empty($metrics) && function_exists('get_option') ) {
			$stored = get_option(homeboy_wordpress_woocommerce_expensive_shipping_metrics_key(), array());
			if ( is_array($stored) ) {
				$metrics = $stored;
			}
		}

		return array_merge(homeboy_wordpress_woocommerce_expensive_shipping_empty_metrics(), $metrics);
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_update_metrics') ) {
	/**
	 * Update fixture metrics atomically within the current PHP request.
	 *
	 * @param array<string,int|float|string> $delta Metric deltas.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_update_metrics(array $delta): void {
		$metrics = homeboy_wordpress_woocommerce_expensive_shipping_metrics();
		foreach ( $delta as $key => $value ) {
			if ( 'deterministic_digest' === $key ) {
				$metrics[ $key ] = (string) $value;
				continue;
			}

			if ( is_numeric($value) ) {
				$metrics[ $key ] = (float) ( $metrics[ $key ] ?? 0 ) + (float) $value;
			}
		}

		foreach ( array( 'calculate_calls', 'packages_seen', 'synthetic_rules', 'cpu_iterations', 'db_queries', 'delay_ms' ) as $integer_key ) {
			$metrics[ $integer_key ] = (int) round( (float) $metrics[ $integer_key ]);
		}

		$GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_metrics'] = $metrics;
		if ( function_exists('update_option') ) {
			update_option(homeboy_wordpress_woocommerce_expensive_shipping_metrics_key(), $metrics, false);
		}
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_int') ) {
	/**
	 * Normalize an integer option with a lower bound.
	 *
	 * @param mixed $value Input value.
	 * @param int   $fallback Default value.
	 * @param int   $minimum Minimum allowed value.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_int($value, int $fallback, int $minimum = 0): int {
		if ( ! is_numeric($value) ) {
			return $fallback;
		}

		return max($minimum, (int) $value);
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_digest') ) {
	/**
	 * Build a deterministic package digest without assuming package shape.
	 *
	 * @param mixed $package WooCommerce shipping package.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_digest($package): string {
		$encoded = wp_json_encode($package);
		if ( false === $encoded ) {
			$encoded = 'unencodable:' . gettype($package);
		}

		return hash('sha256', $encoded);
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_burn_cpu') ) {
	/**
	 * Run deterministic CPU work and return a checksum.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_burn_cpu(string $seed, int $rules, int $iterations): string {
		$checksum = $seed;
		for ( $rule = 0; $rule < $rules; $rule++ ) {
			$rule_seed = $checksum . '|' . $rule;
			for ( $iteration = 0; $iteration < $iterations; $iteration++ ) {
				$checksum = hash('sha256', $rule_seed . '|' . $iteration . '|' . $checksum);
			}
		}

		return $checksum;
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_run_queries') ) {
	/**
	 * Run deterministic local DB queries for query-heavy fixture mode.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_run_queries(int $query_count): void {
		if ( $query_count <= 0 || empty($GLOBALS['wpdb']) ) {
			return;
		}

		global $wpdb;
		for ( $index = 0; $index < $query_count; $index++ ) {
			$wpdb->get_var($wpdb->prepare("SELECT COUNT(*) FROM {$wpdb->options} WHERE option_id >= %d", 0));
		}
	}
}

if ( ! function_exists('homeboy_wordpress_register_woocommerce_expensive_shipping_method') ) {
	/**
	 * Register the deterministic expensive shipping method with WooCommerce.
	 *
	 * @param array<string,mixed> $options Fixture options.
	 * @return bool True when WooCommerce shipping classes are available.
	 */
	function homeboy_wordpress_register_woocommerce_expensive_shipping_method(array $options = array()): bool {
		if ( ! class_exists('WC_Shipping_Method') ) {
			return false;
		}

		$options = homeboy_wordpress_woocommerce_expensive_shipping_defaults($options);
		$GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options'] = $options;
		if ( ! class_exists('Homeboy_WordPress_WooCommerce_Expensive_Shipping_Method') ) {
			require_once __DIR__ . '/woocommerce-expensive-shipping-method.php';
		}

		add_filter('woocommerce_shipping_methods', static function (array $methods) use ($options): array {
			$methods[ (string) $options['method_id'] ] = 'Homeboy_WordPress_WooCommerce_Expensive_Shipping_Method';
			return $methods;
		});

		return true;
	}
}

if ( ! function_exists('homeboy_wordpress_woocommerce_expensive_shipping_payload') ) {
	/**
	 * Return a bench payload with numeric expensive-shipping metrics.
	 *
	 * @param array<string,mixed> $metadata Extra metadata.
	 * @return array<string,array<string,mixed>> Workload payload.
	 */
	function homeboy_wordpress_woocommerce_expensive_shipping_payload(array $metadata = array()): array {
		$metrics          = array();
		$shipping_metrics = homeboy_wordpress_woocommerce_expensive_shipping_metrics();
		foreach ( $shipping_metrics as $key => $value ) {
			if ( is_int($value) || is_float($value) ) {
				$metrics[ 'shipping_' . $key ] = $value;
			}
		}

		return array(
			'metrics'  => $metrics,
			'metadata' => array_merge($metadata, array(
				'woocommerce_expensive_shipping' => $shipping_metrics,
				'checkout_probe_follow_up'       => 'https://github.com/Extra-Chill/homeboy-extensions/issues/1091',
			)),
		);
	}
}
