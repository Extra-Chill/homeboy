# Structured Sidecars

Extension manifests declare structured sidecar support in `structured_sidecars`.

Each key names a stable sidecar contract and each value is a boolean indicating whether the extension can emit that sidecar today.

Supported keys:

- `lint.findings` — writes normalized lint findings to `HOMEBOY_LINT_FINDINGS_FILE`.
- `test.results` — writes normalized test result counts to `HOMEBOY_TEST_RESULTS_FILE`.
- `test.failures` — writes parsed test failure details to `HOMEBOY_TEST_FAILURES_FILE`.
- `test.coverage` — writes normalized coverage data.
- `bench.results` — writes benchmark result envelopes to `HOMEBOY_BENCH_RESULTS_FILE`.
- `trace.results` — writes trace result envelopes to `HOMEBOY_TRACE_RESULTS_FILE`.
- `trace.artifacts` — writes trace artifacts to `HOMEBOY_TRACE_ARTIFACT_DIR`.
- `annotations` — writes inline-review annotation JSON files to `HOMEBOY_ANNOTATIONS_DIR`.

Use `false` for unsupported contracts. Absence means the manifest has not been audited and should not be treated as support.
