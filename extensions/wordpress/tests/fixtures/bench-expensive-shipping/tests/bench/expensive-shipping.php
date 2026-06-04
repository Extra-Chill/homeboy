<?php
/** WP Codebox workload sample for the expensive WooCommerce shipping helper. */

require_once '/homeboy-extension/scripts/bench/lib/woocommerce-expensive-shipping.php';

return function (): array {
    homeboy_wordpress_woocommerce_expensive_shipping_reset_metrics();
    $registered = homeboy_wordpress_register_woocommerce_expensive_shipping_method([
        'synthetic_rules' => 8,
        'cpu_iterations'  => 4,
        'rate_cost'       => 29.95,
    ]);

    if (!$registered || !class_exists('WC_Shipping_Zones') || !function_exists('WC')) {
        return homeboy_wordpress_woocommerce_expensive_shipping_payload([
            'fixture'               => 'bench-expensive-shipping',
            'woocommerce_available' => false,
        ]);
    }

    WC()->cart->calculate_shipping();

    return homeboy_wordpress_woocommerce_expensive_shipping_payload([
        'fixture'               => 'bench-expensive-shipping',
        'woocommerce_available' => true,
    ]);
};
