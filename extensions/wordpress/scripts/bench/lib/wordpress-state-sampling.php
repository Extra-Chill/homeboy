<?php
/**
 * Generic WordPress option/transient sampling helpers for bench workloads.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/wordpress-state-sampling.php
 */

if ( ! function_exists('homeboy_wordpress_bench_sample_option') ) {
	/**
	 * Sample a WordPress option row without exposing SQL details to workloads.
	 *
	 * @param string              $option_name Option name.
	 * @param array<string,mixed> $context Optional sample context.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_sample_option(string $option_name, array $context = array()): array {
		$row = homeboy_wordpress_bench_read_option_row($option_name);
		return homeboy_wordpress_bench_format_option_sample('option', $option_name, $option_name, $row, $context);
	}
}

if ( ! function_exists('homeboy_wordpress_bench_sample_transient') ) {
	/**
	 * Sample a WordPress transient option row.
	 *
	 * @param string              $transient Transient name without the _transient_ prefix.
	 * @param array<string,mixed> $context Optional sample context. Pass network=true for site transients.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_sample_transient(string $transient, array $context = array()): array {
		$network     = ! empty($context['network']);
		$option_name = ( $network ? '_site_transient_' : '_transient_' ) . $transient;
		$row         = $network ? homeboy_wordpress_bench_read_site_option_row($option_name) : homeboy_wordpress_bench_read_option_row($option_name);

		$sample                    = homeboy_wordpress_bench_format_option_sample('transient', $transient, $option_name, $row, $context);
		$sample['transient_scope'] = $network ? 'site' : 'single-site';

		return $sample;
	}
}

if ( ! function_exists('homeboy_wordpress_bench_sample_delta') ) {
	/**
	 * Return a small numeric delta between two option/transient samples.
	 *
	 * @param array<string,mixed> $before Earlier sample.
	 * @param array<string,mixed> $after Later sample.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_sample_delta(array $before, array $after): array {
		return array(
			'kind'                      => $after['kind'] ?? $before['kind'] ?? 'option',
			'name'                      => $after['name'] ?? $before['name'] ?? '',
			'option_name'               => $after['option_name'] ?? $before['option_name'] ?? '',
			'before_exists'             => (bool) ( $before['exists'] ?? false ),
			'after_exists'              => (bool) ( $after['exists'] ?? false ),
			'exists_changed'            => (bool) ( $before['exists'] ?? false ) !== (bool) ( $after['exists'] ?? false ),
			'serialized_bytes_delta'    => homeboy_wordpress_bench_nullable_number_delta($before['serialized_bytes'] ?? null, $after['serialized_bytes'] ?? null),
			'array_entry_count_delta'   => homeboy_wordpress_bench_nullable_number_delta($before['array_entry_count'] ?? null, $after['array_entry_count'] ?? null),
			'before_sample_index'       => $before['sample_index'] ?? null,
			'after_sample_index'        => $after['sample_index'] ?? null,
			'before_sampled_at_unix_ms' => $before['sampled_at_unix_ms'] ?? null,
			'after_sampled_at_unix_ms'  => $after['sampled_at_unix_ms'] ?? null,
		);
	}
}

if ( ! function_exists('homeboy_wordpress_bench_read_option_row') ) {
	/**
	 * Read a raw option row through wpdb.
	 *
	 * @param string $option_name Option name.
	 * @return array<string,string>|null
	 */
	function homeboy_wordpress_bench_read_option_row(string $option_name): ?array {
		if ( '' === $option_name || empty($GLOBALS['wpdb']) ) {
			return null;
		}

		global $wpdb;
		if ( empty($wpdb->options) || ! method_exists($wpdb, 'prepare') || ! method_exists($wpdb, 'get_row') ) {
			return null;
		}

		$format = defined('ARRAY_A') ? ARRAY_A : 'ARRAY_A';
		$row    = $wpdb->get_row(
			$wpdb->prepare("SELECT option_value FROM {$wpdb->options} WHERE option_name = %s LIMIT 1", $option_name),
			$format
		);

		return is_array($row) ? $row : null;
	}
}

if ( ! function_exists('homeboy_wordpress_bench_read_site_option_row') ) {
	/**
	 * Read a raw site option row through wpdb.
	 *
	 * @param string $option_name Site option name.
	 * @return array<string,string>|null
	 */
	function homeboy_wordpress_bench_read_site_option_row(string $option_name): ?array {
		if ( ! function_exists('is_multisite') || ! is_multisite() ) {
			return homeboy_wordpress_bench_read_option_row($option_name);
		}
		if ( '' === $option_name || empty($GLOBALS['wpdb']) ) {
			return null;
		}

		global $wpdb;
		if ( empty($wpdb->sitemeta) || ! method_exists($wpdb, 'prepare') || ! method_exists($wpdb, 'get_row') ) {
			return null;
		}

		$network_id = function_exists('get_current_network_id') ? (int) get_current_network_id() : 1;
		$format     = defined('ARRAY_A') ? ARRAY_A : 'ARRAY_A';
		$row        = $wpdb->get_row(
			$wpdb->prepare("SELECT meta_value AS option_value FROM {$wpdb->sitemeta} WHERE site_id = %d AND meta_key = %s LIMIT 1", $network_id, $option_name),
			$format
		);

		return is_array($row) ? $row : null;
	}
}

if ( ! function_exists('homeboy_wordpress_bench_format_option_sample') ) {
	/**
	 * Format a raw option row as a bench artifact-friendly sample.
	 *
	 * @param string                    $kind Sample kind.
	 * @param string                    $name Public sample name.
	 * @param string                    $option_name Backing option row name.
	 * @param array<string,string>|null $row Raw option row.
	 * @param array<string,mixed>       $context Optional sample context.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_format_option_sample(string $kind, string $name, string $option_name, ?array $row, array $context): array {
		$exists        = null !== $row && array_key_exists('option_value', $row);
		$raw_value     = $exists ? (string) $row['option_value'] : null;
		$value         = $exists ? homeboy_wordpress_bench_maybe_unserialize($raw_value) : null;
		$sampled_at_ms = array_key_exists('sampled_at_unix_ms', $context)
			? $context['sampled_at_unix_ms']
			: (int) floor(microtime(true) * 1000);

		$sample = array(
			'kind'               => $kind,
			'name'               => $name,
			'option_name'        => $option_name,
			'exists'             => $exists,
			'missing'            => ! $exists,
			'serialized_bytes'   => $exists ? strlen($raw_value) : null,
			'value_type'         => $exists ? gettype($value) : null,
			'array_entry_count'  => is_array($value) ? count($value) : null,
			'sampled_at_unix_ms' => is_numeric($sampled_at_ms) ? (int) $sampled_at_ms : null,
		);

		if ( array_key_exists('sample_index', $context) ) {
			$sample['sample_index'] = is_numeric($context['sample_index']) ? (int) $context['sample_index'] : $context['sample_index'];
		}

		if ( isset($context['label']) && is_string($context['label']) && '' !== $context['label'] ) {
			$sample['label'] = $context['label'];
		}

		return $sample;
	}
}

if ( ! function_exists('homeboy_wordpress_bench_nullable_number_delta') ) {
	/**
	 * Return after-before when both values are numeric; otherwise null.
	 *
	 * @param mixed $before Earlier value.
	 * @param mixed $after Later value.
	 * @return int|float|null
	 */
	function homeboy_wordpress_bench_nullable_number_delta($before, $after) {
		if ( ! is_numeric($before) || ! is_numeric($after) ) {
			return null;
		}

		return $after - $before;
	}
}

if ( ! function_exists('homeboy_wordpress_bench_maybe_unserialize') ) {
	/**
	 * Unserialize WordPress option payloads without requiring WordPress in smoke tests.
	 *
	 * @param string $value Raw option_value payload.
	 * @return mixed
	 */
	function homeboy_wordpress_bench_maybe_unserialize(string $value) {
		if ( function_exists('maybe_unserialize') ) {
			return maybe_unserialize($value);
		}

		return $value;
	}
}
