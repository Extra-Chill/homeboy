<?php
/**
 * Generic JSON artifact helpers for WordPress bench workloads.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/wordpress-bench-artifacts.php
 */

if ( ! function_exists('homeboy_bench_write_json_artifact') ) {
	/**
	 * Write a JSON artifact under the shared bench state directory.
	 *
	 * @param string $scenario Scenario id used as the artifact subdirectory.
	 * @param string $name Artifact name used as the JSON filename.
	 * @param mixed  $payload JSON-serializable artifact payload.
	 * @return array<string,string> Standard bench artifact descriptor.
	 */
	function homeboy_bench_write_json_artifact(string $scenario, string $name, $payload): array {
		$shared_state = homeboy_bench_shared_state_path();
		$scenario_slug = homeboy_bench_artifact_slug($scenario);
		$name_slug = homeboy_bench_artifact_slug(preg_replace('/\.json$/i', '', $name) ?? $name);

		if ( '' === $scenario_slug ) {
			throw new InvalidArgumentException('Bench artifact scenario must contain at least one safe filename character.');
		}
		if ( '' === $name_slug ) {
			throw new InvalidArgumentException('Bench artifact name must contain at least one safe filename character.');
		}

		$relative_path = 'artifacts/' . $scenario_slug . '/' . $name_slug . '.json';
		$artifact_path = rtrim($shared_state, '/\\') . '/' . $relative_path;
		$artifact_dir = dirname($artifact_path);

		if ( ! is_dir($artifact_dir) && ! mkdir($artifact_dir, 0777, true) && ! is_dir($artifact_dir) ) {
			throw new RuntimeException('Failed to create bench artifact directory: ' . $artifact_dir);
		}

		$json = homeboy_bench_json_encode($payload);
		$tmp_path = $artifact_path . '.tmp.' . getmypid() . '.' . bin2hex(random_bytes(6));
		if ( false === file_put_contents($tmp_path, $json . "\n", LOCK_EX) ) {
			throw new RuntimeException('Failed to write bench artifact: ' . $artifact_path);
		}
		if ( ! rename($tmp_path, $artifact_path) ) {
			@unlink($tmp_path);
			throw new RuntimeException('Failed to finalize bench artifact: ' . $artifact_path);
		}

		return array(
			'path' => '/bench-shared-state/' . $relative_path,
			'kind' => 'json',
		);
	}
}

if ( ! function_exists('homeboy_bench_shared_state_path') ) {
	/**
	 * Resolve the shared state path exposed to the workload runtime.
	 *
	 * @return string Absolute path inside the workload runtime.
	 */
	function homeboy_bench_shared_state_path(): string {
		$env_value = getenv('HOMEBOY_BENCH_SHARED_STATE');
		$shared_state = is_string($env_value) ? trim($env_value) : '';
		if ( '' === $shared_state && defined('HOMEBOY_BENCH_SHARED_STATE') ) {
			$shared_state = trim((string) constant('HOMEBOY_BENCH_SHARED_STATE'));
		}

		if ( '' === $shared_state ) {
			throw new RuntimeException('HOMEBOY_BENCH_SHARED_STATE is required to write bench artifacts.');
		}

		return $shared_state;
	}
}

if ( ! function_exists('homeboy_bench_artifact_slug') ) {
	/**
	 * Convert workload-provided ids to safe path segments.
	 *
	 * @param string $value Raw path segment.
	 * @return string Safe path segment.
	 */
	function homeboy_bench_artifact_slug(string $value): string {
		$slug = strtolower(trim($value));
		$slug = preg_replace('/[^a-z0-9._-]+/', '-', $slug) ?? '';
		$slug = trim($slug, '.-_');

		return $slug;
	}
}

if ( ! function_exists('homeboy_bench_json_encode') ) {
	/**
	 * Encode artifact payloads with WordPress when available.
	 *
	 * @param mixed $payload JSON-serializable payload.
	 * @return string Encoded JSON.
	 */
	function homeboy_bench_json_encode($payload): string {
		$flags = JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES;
		$json = function_exists('wp_json_encode')
			? wp_json_encode($payload, $flags)
			: json_encode($payload, $flags);

		if ( ! is_string($json) || '' === $json ) {
			$message = function_exists('json_last_error_msg') ? json_last_error_msg() : 'unknown JSON encoding error';
			throw new RuntimeException('Failed to encode bench artifact JSON: ' . $message);
		}

		return $json;
	}
}
