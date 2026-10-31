<?php
/** Smoke test for generic WordPress bench artifact helpers. */

require_once __DIR__ . '/../scripts/bench/lib/wordpress-bench-artifacts.php';

$root = sys_get_temp_dir() . '/homeboy-wordpress-bench-artifacts-' . uniqid('', true);
if ( ! mkdir($root, 0777, true) && ! is_dir($root) ) {
	fwrite(STDERR, "Failed to create temp root.\n");
	exit(1);
}

putenv('HOMEBOY_BENCH_SHARED_STATE=' . $root);

$descriptor = homeboy_bench_write_json_artifact('Scenario One', 'step-series', array(
	'ok'    => true,
	'rows'  => array(array('elapsed_ms' => 12.3)),
	'route' => '/wp-admin/',
));

$expected_descriptor = array(
	'path' => '/bench-shared-state/artifacts/scenario-one/step-series.json',
	'kind' => 'json',
);
if ( $expected_descriptor !== $descriptor ) {
	fwrite(STDERR, "Expected standard artifact descriptor.\n");
	exit(1);
}

$artifact_path = $root . '/artifacts/scenario-one/step-series.json';
if ( ! is_file($artifact_path) ) {
	fwrite(STDERR, "Expected artifact file to be written under shared state.\n");
	exit(1);
}

$payload = json_decode((string) file_get_contents($artifact_path), true);
if ( ! is_array($payload) || true !== $payload['ok'] || '/wp-admin/' !== $payload['route'] ) {
	fwrite(STDERR, "Expected JSON payload to round-trip.\n");
	exit(1);
}

$sanitized_descriptor = homeboy_bench_write_json_artifact('../Nested Scenario', 'unsafe/name.json', array('ok' => true));
if ( '/bench-shared-state/artifacts/nested-scenario/unsafe-name.json' !== $sanitized_descriptor['path'] ) {
	fwrite(STDERR, "Expected scenario and name to be sanitized into safe artifact path segments.\n");
	exit(1);
}

putenv('HOMEBOY_BENCH_SHARED_STATE');
$failed_without_shared_state = false;
try {
	homeboy_bench_write_json_artifact('scenario', 'missing-shared-state', array());
} catch ( RuntimeException $exception ) {
	$failed_without_shared_state = false !== strpos($exception->getMessage(), 'HOMEBOY_BENCH_SHARED_STATE');
}
if ( ! $failed_without_shared_state ) {
	fwrite(STDERR, "Expected missing HOMEBOY_BENCH_SHARED_STATE to fail clearly.\n");
	exit(1);
}

echo "wordpress bench artifact helper smoke passed\n";
