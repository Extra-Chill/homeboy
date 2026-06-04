<?php
/**
 * Deterministic local-only expensive WooCommerce shipping method for bench workloads.
 */

if ( class_exists('WC_Shipping_Method') && ! class_exists('Homeboy_WordPress_WooCommerce_Expensive_Shipping_Method') ) {
	/**
	 * Shipping method that burns deterministic local resources before adding a rate.
	 */
	class Homeboy_WordPress_WooCommerce_Expensive_Shipping_Method extends WC_Shipping_Method {
		/**
		 * Constructor.
		 */
		public function __construct() {
			$options = homeboy_wordpress_woocommerce_expensive_shipping_defaults(
				isset($GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options']) && is_array($GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options'])
					? $GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options']
					: array()
			);

			$this->id                 = (string) $options['method_id'];
			$this->method_title       = (string) $options['method_title'];
			$this->method_description = 'Deterministic local-only expensive shipping method for Homeboy/WP Codebox bench workloads.';
			$this->enabled            = 'yes';
			$this->title              = (string) $options['rate_label'];
		}

		/**
		 * Calculate an intentionally expensive deterministic shipping rate.
		 *
		 * @param array<string,mixed> $package Shipping package.
		 */
		public function calculate_shipping($package = array()): void {
			$options        = homeboy_wordpress_woocommerce_expensive_shipping_defaults(
				isset($GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options']) && is_array($GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options'])
					? $GLOBALS['homeboy_wordpress_woocommerce_expensive_shipping_options']
					: array()
			);
			$rules          = homeboy_wordpress_woocommerce_expensive_shipping_int($options['synthetic_rules'], 250);
			$cpu_iterations = homeboy_wordpress_woocommerce_expensive_shipping_int($options['cpu_iterations'], 50);
			$db_queries     = homeboy_wordpress_woocommerce_expensive_shipping_int($options['db_queries'], 0);
			$delay_ms       = homeboy_wordpress_woocommerce_expensive_shipping_int($options['delay_ms'], 0);
			$started        = function_exists('hrtime') ? hrtime(true) : microtime(true);

			$digest = homeboy_wordpress_woocommerce_expensive_shipping_burn_cpu(
				homeboy_wordpress_woocommerce_expensive_shipping_digest($package),
				$rules,
				$cpu_iterations
			);
			homeboy_wordpress_woocommerce_expensive_shipping_run_queries($db_queries);
			if ( $delay_ms > 0 ) {
				usleep($delay_ms * 1000);
			}

			$elapsed_ms = function_exists('hrtime')
				? ( hrtime(true) - $started ) / 1000000
				: ( microtime(true) - $started ) * 1000;

			homeboy_wordpress_woocommerce_expensive_shipping_update_metrics(array(
				'calculate_calls'      => 1,
				'packages_seen'        => 1,
				'synthetic_rules'      => $rules,
				'cpu_iterations'       => $rules * $cpu_iterations,
				'db_queries'           => $db_queries,
				'delay_ms'             => $delay_ms,
				'elapsed_ms'           => $elapsed_ms,
				'deterministic_digest' => $digest,
			));

			$this->add_rate(array(
				'id'    => $this->id,
				'label' => (string) $options['rate_label'],
				'cost'  => (float) $options['rate_cost'],
			));
		}
	}
}
