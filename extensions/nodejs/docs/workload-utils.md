# Node.js Workload Utilities

Node benchmark workloads can import the product-neutral helper exported by
`HOMEBOY_NODEJS_WORKLOAD_UTILS`.

```js
const { metric, runCommand, writeJson } = await import(process.env.HOMEBOY_NODEJS_WORKLOAD_UTILS);
```

The module keeps common workload plumbing in one place without encoding product
concepts:

- `setting(key, fallback)` resolves `HOMEBOY_SETTINGS_JSON` first, then `HOMEBOY_SETTINGS_<KEY>`.
- `metric(value, fallback)` coerces finite numeric values.
- `runCommand(command, args, options)` runs a subprocess with redacted output by default.
- `writeJson(file, data)` and `writeText(file, data)` create parent directories and redact common secrets.
- `runId(name)` creates a sanitized run identifier.
- `artifactDir(name)` returns a directory under `HOMEBOY_BENCH_SHARED_STATE` or the OS temp directory.
- `createArtifactContext(options)` wraps the existing bench artifact context helper.
- `percentile(values, pct)` uses R-7 linear interpolation.

Use `{ redact: false }` with `runCommand()`, `writeJson()`, or `writeText()` only when a workload needs raw output for parsing before writing a redacted artifact.
