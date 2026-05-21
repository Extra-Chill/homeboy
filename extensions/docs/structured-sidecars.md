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

## `lint.findings` v1

`HOMEBOY_LINT_FINDINGS_FILE` contains a JSON array of lint finding objects. Emit fields when known; use `null` for optional unknown values when a stable field is useful to consumers.

- `id` — stable finding identity derived from tool, file, location, rule, and message.
- `file` — repository-relative path when available.
- `line` — 1-based line number when available.
- `column` — 1-based column number when available.
- `severity` — `error` or `warning`.
- `source` — producing tool, for example `eslint`, `phpcs`, `phpstan`, `rustfmt`, `clippy`, `swiftlint`, `gofmt`, or `go vet`.
- `code` — tool rule or diagnostic code.
- `category` — coarse grouping such as `style`, `format`, or `correctness`.
- `message` — human-readable diagnostic message.
- `fixable` — boolean indicating whether the tool can apply a fix automatically.
- `fingerprint` — stable hash for unchanged findings.
- `excerpt` — bounded source line excerpt when available.

## `test.failures` v1

`HOMEBOY_TEST_FAILURES_FILE` contains a JSON array of test failure objects.

- `test_id` — stable test identifier, usually package/module plus test name.
- `suite` — suite, package, class, or runner grouping when available.
- `file` — repository-relative test file when available.
- `line` — 1-based failure line when available.
- `message` — human-readable failure summary.
- `failure_type` — `test_failure` for assertion/test failures or `infrastructure` for runner/package/setup failures.
- `fingerprint` — stable hash for unchanged failures.
- `stdout_excerpt` — bounded output excerpt relevant to the failure.
- `stderr_excerpt` — bounded stderr excerpt relevant to the failure.
