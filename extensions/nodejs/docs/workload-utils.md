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
- `runPackageScriptBench(options)` runs a `package.json` script with optional spec args and returns the standard bench workload shape: `{ metrics, artifacts, metadata }`.
- `writeJson(file, data)` and `writeText(file, data)` create parent directories and redact common secrets.
- `runId(name)` creates a sanitized run identifier.
- `artifactDir(name)` returns a directory under `HOMEBOY_BENCH_SHARED_STATE` or the OS temp directory.
- `createArtifactContext(options)` wraps the existing bench artifact context helper.
- `percentile(values, pct)` uses R-7 linear interpolation.

Use `{ redact: false }` with `runCommand()`, `writeJson()`, or `writeText()` only when a workload needs raw output for parsing before writing a redacted artifact.

## WordPress/Codebox Visual Parity Workloads

`runWordPressCodeboxVisualParityWorkload()` is the WordPress/Codebox-specific
primitive for visual comparison workloads. Callers explicitly provide the Codebox
backend, a source URL or local source path, a candidate WordPress context,
viewport/threshold settings, and receive a normalized
`homeboy/VisualParityArtifact/v1` artifact while preserving the underlying
Codebox artifact references.

```js
const { runWordPressCodeboxVisualParityWorkload } = await import(process.env.HOMEBOY_NODEJS_WORKLOAD_UTILS);

export default async function () {
  return runWordPressCodeboxVisualParityWorkload({
    id: 'homepage-parity',
    backend: { codeboxCli: process.env.CODEBOX_CLI },
    source: {
      path: './dist/site',
      ref: process.env.GITHUB_SHA,
      label: 'static-source',
      port: 4173,
    },
    candidate: {
      url: '/',
      label: 'wordpress-candidate',
      context: { runtime: 'playground' },
      recipe: {
        runtime: { wp: 'latest' },
        inputs: { mounts: [] },
      },
    },
    viewport: { width: 1280, height: 1600 },
    threshold: 0.015,
    pixelThreshold: 0.1,
    waitFor: 'domcontentloaded',
  });
}
```

The generic Node.js bench runner does not auto-discover or export WordPress
helpers. Callers own backend discovery, site-specific import steps, scoring, PR
comments, retry policy, and reviewer gates. Homeboy Extensions owns the reusable
recipe invocation and artifact normalization shape:

```json
{
  "schema": "homeboy/VisualParityArtifact/v1",
  "source": { "label": "static-source", "ref": "...", "path": "...", "url": "..." },
  "candidate": { "label": "wordpress-candidate", "ref": "...", "url": "/", "context": {} },
  "summary": {
    "status": "passed",
    "pass": true,
    "threshold": 0.015,
    "mismatch_ratio": 0,
    "mismatch_pixels": 0,
    "total_pixels": 2048000,
    "dimension_mismatch": false,
    "region_count": 0
  },
  "artifacts": {
    "directory": "...",
    "visual_diff": "files/browser/visual-compare/visual-diff.json",
    "source_screenshot": "files/browser/visual-compare/source.png",
    "candidate_screenshot": "files/browser/visual-compare/candidate.png",
    "diff_screenshot": "files/browser/visual-compare/diff.png"
  }
}
```

Typed setting helpers use the same precedence as `setting()`: values in
`HOMEBOY_SETTINGS_JSON` win when the key exists and is not `null`; otherwise the
helper reads `HOMEBOY_SETTINGS_<KEY>`, or a custom `options.prefix` when one is
provided. Invalid typed values return the helper fallback instead of throwing so
workloads can keep a single defaulting path.

## Package Script Bench Helper

Use `runPackageScriptBench()` when a workload only needs to benchmark an existing
package script or a subset of specs accepted by that script.

```js
const { runPackageScriptBench } = await import(process.env.HOMEBOY_NODEJS_WORKLOAD_UTILS);

export default async function () {
  return runPackageScriptBench({
    script: 'test:e2e',
    specs: ['specs/editor.spec.ts'],
    timeoutMs: 120_000,
  });
}
```

The helper detects `pnpm`, `yarn`, or `npm` from the component root, validates
that the script exists, forwards `args` followed by `specs`, writes a redacted
JSON artifact with command output, and returns neutral metrics such as
`package_script_elapsed_ms`, `package_script_exit_code`, and
`package_script_spec_count`.
