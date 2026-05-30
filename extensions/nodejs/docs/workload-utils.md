# Node.js Workload Utilities

Node benchmark workloads can import the product-neutral helper exported by
`HOMEBOY_NODEJS_WORKLOAD_UTILS`.

```js
const { metric, runCommand, writeJson } = await import(process.env.HOMEBOY_NODEJS_WORKLOAD_UTILS);
```

The module keeps common workload plumbing in one place without encoding product
concepts:

- `setting(key, fallback)` resolves `HOMEBOY_SETTINGS_JSON` first, then `HOMEBOY_SETTINGS_<KEY>`, and returns the resolved value as a string.
- `settingInt(key, fallback, options)` resolves the same sources and returns an integer, or `fallback` when the value is missing, not an integer, below `options.min`, or above `options.max`.
- `settingBool(key, fallback, options)` accepts JSON booleans, `1`/`0`, and string booleans such as `true`/`false`, `yes`/`no`, and `on`/`off`; invalid values return `fallback`.
- `settingList(key, fallback, options)` accepts JSON arrays or splits string values on `options.separator` (`,` by default), trims entries by default, and drops empty entries unless `options.keepEmpty` is true.
- `settingJson(key, fallback, options)` returns JSON object/array values from `HOMEBOY_SETTINGS_JSON` as-is, parses string/env values as JSON, and returns `fallback` when parsing fails.
- `expandHome(value, options)` expands `~` and `~/...` using `options.homeDir` or the current OS home directory.
- `resolvePath(value, options)` expands `~`, preserves absolute paths, and resolves relative paths from `options.baseDir`, `HOMEBOY_COMPONENT_PATH`, or the current working directory.
- `metric(value, fallback)` coerces finite numeric values.
- `runCommand(command, args, options)` runs a subprocess with redacted output by default.
- `writeJson(file, data)` and `writeText(file, data)` create parent directories and redact common secrets.
- `runId(name)` creates a sanitized run identifier.
- `artifactDir(name)` returns a directory under `HOMEBOY_BENCH_SHARED_STATE` or the OS temp directory.
- `createArtifactContext(options)` wraps the existing bench artifact context helper.
- `percentile(values, pct)` uses R-7 linear interpolation.

Use `{ redact: false }` with `runCommand()`, `writeJson()`, or `writeText()` only when a workload needs raw output for parsing before writing a redacted artifact.

Typed setting helpers use the same precedence as `setting()`: values in
`HOMEBOY_SETTINGS_JSON` win when the key exists and is not `null`; otherwise the
helper reads `HOMEBOY_SETTINGS_<KEY>`, or a custom `options.prefix` when one is
provided. Invalid typed values return the helper fallback instead of throwing so
workloads can keep a single defaulting path.
