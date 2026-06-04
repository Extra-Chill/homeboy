<?php
/** Smoke test for the deterministic expensive WooCommerce shipping helper. */

$homeboy_expensive_shipping_options_store = [];
$homeboy_expensive_shipping_filters = [];

function wp_json_encode($data) {
    return json_encode($data);
}

function get_option($key, $default = false) {
    global $homeboy_expensive_shipping_options_store;
    return array_key_exists($key, $homeboy_expensive_shipping_options_store) ? $homeboy_expensive_shipping_options_store[$key] : $default;
}

function update_option($key, $value, $autoload = null) {
    global $homeboy_expensive_shipping_options_store;
    $homeboy_expensive_shipping_options_store[$key] = $value;
    return true;
}

function add_filter($hook_name, $callback) {
    global $homeboy_expensive_shipping_filters;
    $homeboy_expensive_shipping_filters[$hook_name][] = $callback;
    return true;
}

function homeboy_expensive_shipping_apply_filters($hook_name, $value) {
    global $homeboy_expensive_shipping_filters;
    foreach ($homeboy_expensive_shipping_filters[$hook_name] ?? [] as $callback) {
        $value = $callback($value);
    }
    return $value;
}

class WC_Shipping_Method {
    public $id = '';
    public $method_title = '';
    public $method_description = '';
    public $enabled = 'yes';
    public $title = '';
    public $rates = [];

    public function add_rate($rate) {
        $this->rates[] = $rate;
    }
}

require_once __DIR__ . '/../scripts/bench/lib/woocommerce-expensive-shipping.php';

homeboy_wordpress_woocommerce_expensive_shipping_reset_metrics();
$registered = homeboy_wordpress_register_woocommerce_expensive_shipping_method([
    'method_id'       => 'fixture_expensive_shipping',
    'synthetic_rules' => 4,
    'cpu_iterations'  => 3,
    'rate_cost'       => 42,
]);

if (!$registered) {
    fwrite(STDERR, "Expected expensive shipping method to register.\n");
    exit(1);
}

$methods = homeboy_expensive_shipping_apply_filters('woocommerce_shipping_methods', []);
if (($methods['fixture_expensive_shipping'] ?? '') !== 'Homeboy_WordPress_WooCommerce_Expensive_Shipping_Method') {
    fwrite(STDERR, "Expected expensive shipping method in WooCommerce methods filter.\n");
    exit(1);
}

$method = new Homeboy_WordPress_WooCommerce_Expensive_Shipping_Method();
$method->calculate_shipping([
    'contents'    => [['product_id' => 123, 'quantity' => 2]],
    'destination' => ['country' => 'US', 'state' => 'CA', 'postcode' => '94110'],
]);

$metrics = homeboy_wordpress_woocommerce_expensive_shipping_metrics();
$expected = [
    'calculate_calls' => 1,
    'packages_seen'   => 1,
    'synthetic_rules' => 4,
    'cpu_iterations'  => 12,
];
foreach ($expected as $key => $value) {
    if (($metrics[$key] ?? null) !== $value) {
        fwrite(STDERR, "Expected {$key}={$value}, got " . var_export($metrics[$key] ?? null, true) . "\n");
        exit(1);
    }
}

if (count($method->rates) !== 1 || (float) $method->rates[0]['cost'] !== 42.0) {
    fwrite(STDERR, "Expected one deterministic shipping rate with configured cost.\n");
    exit(1);
}

$payload = homeboy_wordpress_woocommerce_expensive_shipping_payload(['fixture' => 'smoke']);
if (($payload['metrics']['shipping_calculate_calls'] ?? null) !== 1) {
    fwrite(STDERR, "Expected payload shipping_calculate_calls metric.\n");
    exit(1);
}
if (($payload['metadata']['checkout_probe_follow_up'] ?? '') !== 'https://github.com/Extra-Chill/homeboy-extensions/issues/1091') {
    fwrite(STDERR, "Expected checkout probe follow-up metadata.\n");
    exit(1);
}

echo "woocommerce expensive shipping helper smoke passed\n";
