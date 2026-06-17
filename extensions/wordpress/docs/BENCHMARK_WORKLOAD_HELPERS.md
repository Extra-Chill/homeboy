# WordPress Benchmark Workload Helpers

WordPress benchmark workloads can require reusable PHP helpers from the mounted Homeboy extension path. For WP Codebox benchmark runs, the extension is available at `/homeboy-extension`.

## JSON Artifacts

Use `homeboy_bench_write_json_artifact( $scenario, $name, $payload )` when a workload needs to persist structured evidence that is too large or detailed for numeric metrics.

```php
<?php
require_once '/homeboy-extension/scripts/bench/lib/wordpress-bench-artifacts.php';

return function (): array {
	$artifact = homeboy_bench_write_json_artifact(
		'route-latency',
		'step-series',
		array(
			'rows' => array(
				array('route' => '/', 'elapsed_ms' => 42.5),
			),
		)
	);

	return array(
		'metrics' => array(
			'route_latency_samples' => 1,
		),
		'artifacts' => array(
			'step_series' => $artifact,
		),
	);
};
```

The helper reads `HOMEBOY_BENCH_SHARED_STATE`, creates `artifacts/<scenario>/`, writes pretty-printed JSON, and returns a descriptor like:

```json
{
  "path": "artifacts/route-latency/step-series.json",
  "kind": "json",
  "label": "step-series",
  "mime_type": "application/json"
}
```

Scenario and name values are sanitized into safe path segments. The helper throws a `RuntimeException` when shared state is unavailable or the artifact cannot be written.
