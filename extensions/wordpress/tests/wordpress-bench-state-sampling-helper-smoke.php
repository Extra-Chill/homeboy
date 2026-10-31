<?php
/** Smoke test for generic WordPress bench state sampling helpers. */

if ( ! defined('ARRAY_A') ) {
	define('ARRAY_A', 'ARRAY_A');
}

class Homeboy_WordPress_Bench_State_Sampling_WPDB {
	/** @var string */
	public $options = 'wp_options';

	/** @var string */
	public $sitemeta = 'wp_sitemeta';

	/** @var array<string,string> */
	private $rows;

	/** @var array<string,string> */
	private $site_rows;

	/**
	 * @param array<string,string> $rows Rows keyed by option_name.
	 * @param array<string,string> $site_rows Site rows keyed by meta_key.
	 */
	public function __construct(array $rows, array $site_rows = array()) {
		$this->rows      = $rows;
		$this->site_rows = $site_rows;
	}

	public function prepare($query, ...$args) {
		return array('query' => $query, 'args' => $args);
	}

	public function get_row($prepared, $format = ARRAY_A) {
		$option_name = $prepared['args'][0] ?? '';
		if ( count($prepared['args']) > 1 ) {
			$option_name = $prepared['args'][1];
			if ( ! array_key_exists($option_name, $this->site_rows) ) {
				return null;
			}

			return array('option_value' => $this->site_rows[ $option_name ]);
		}

		if ( ! array_key_exists($option_name, $this->rows) ) {
			return null;
		}

		return array('option_value' => $this->rows[ $option_name ]);
	}
}

function maybe_unserialize($value) {
	$unserialized = @unserialize($value);
	if ( false !== $unserialized || 'b:0;' === $value ) {
		return $unserialized;
	}

	return $value;
}

function is_multisite() {
	return true;
}

function get_current_network_id() {
	return 1;
}

$GLOBALS['wpdb'] = new Homeboy_WordPress_Bench_State_Sampling_WPDB(array(
	'plain_option'             => 'plain value',
	'array_option'             => serialize(array('one' => 1, 'two' => 2, 'three' => 3)),
	'_transient_fixture_cache'  => serialize(array('alpha' => true, 'beta' => false)),
), array(
	'_site_transient_network_fixture_cache' => serialize(array('global' => true)),
));

require_once __DIR__ . '/../scripts/bench/lib/wordpress-state-sampling.php';

$plain = homeboy_wordpress_bench_sample_option('plain_option', array('sample_index' => 7, 'sampled_at_unix_ms' => 123456));
if ( true !== $plain['exists'] || false !== $plain['missing'] ) {
	fwrite(STDERR, "Expected existing plain option sample.\n");
	exit(1);
}
if ( 'string' !== $plain['value_type'] || null !== $plain['array_entry_count'] ) {
	fwrite(STDERR, "Expected plain option to report string type and no array count.\n");
	exit(1);
}
if ( strlen('plain value') !== $plain['serialized_bytes'] || 7 !== $plain['sample_index'] || 123456 !== $plain['sampled_at_unix_ms'] ) {
	fwrite(STDERR, "Expected plain option bytes and sample context to round-trip.\n");
	exit(1);
}

$array = homeboy_wordpress_bench_sample_option('array_option', array('sample_index' => 8));
if ( 'array' !== $array['value_type'] || 3 !== $array['array_entry_count'] ) {
	fwrite(STDERR, "Expected serialized array option to report array entry count.\n");
	exit(1);
}

$missing = homeboy_wordpress_bench_sample_option('missing_option');
if ( false !== $missing['exists'] || true !== $missing['missing'] || null !== $missing['serialized_bytes'] ) {
	fwrite(STDERR, "Expected missing option sample with null byte size.\n");
	exit(1);
}

$transient = homeboy_wordpress_bench_sample_transient('fixture_cache', array('sample_index' => 9));
if ( 'transient' !== $transient['kind'] || '_transient_fixture_cache' !== $transient['option_name'] ) {
	fwrite(STDERR, "Expected transient helper to resolve backing option name.\n");
	exit(1);
}
if ( 2 !== $transient['array_entry_count'] || 'single-site' !== $transient['transient_scope'] ) {
	fwrite(STDERR, "Expected transient helper to report array count and scope.\n");
	exit(1);
}

$site_transient = homeboy_wordpress_bench_sample_transient('network_fixture_cache', array('network' => true));
if ( '_site_transient_network_fixture_cache' !== $site_transient['option_name'] || 'site' !== $site_transient['transient_scope'] ) {
	fwrite(STDERR, "Expected site transient helper to resolve backing site option name and scope.\n");
	exit(1);
}
if ( 1 !== $site_transient['array_entry_count'] ) {
	fwrite(STDERR, "Expected site transient helper to read from sitemeta rows.\n");
	exit(1);
}

$delta = homeboy_wordpress_bench_sample_delta($plain, $array);
if ( strlen(serialize(array('one' => 1, 'two' => 2, 'three' => 3))) - strlen('plain value') !== $delta['serialized_bytes_delta'] ) {
	fwrite(STDERR, "Expected serialized byte delta.\n");
	exit(1);
}
if ( null !== $delta['array_entry_count_delta'] || false !== $delta['exists_changed'] ) {
	fwrite(STDERR, "Expected null array delta and unchanged existence.\n");
	exit(1);
}

echo "wordpress bench state sampling helper smoke passed\n";
