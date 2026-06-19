# `homeboy report`

Render reports from Homeboy structured output artifacts.

## Synopsis

```sh
homeboy report <COMMAND>
```

## Subcommands

- `failure-digest` — render a markdown failure digest from Homeboy command output JSON files
- `performance-digest` — render resource, budget, baseline-health, preview, and Lab offload details from Homeboy run artifacts
- `bench-coverage` — report list-only benchmark coverage for hot command paths
- `browser-evidence-compare` — compare baseline/candidate browser evidence artifact bundles, optionally including visual screenshot diffs through a declared provider

## Performance Digest

```sh
homeboy report performance-digest \
  --output-dir <DIR> \
  [--metadata-json <JSON_OR_FILE>] \
  [--run-url <URL>] \
  [--min-samples 3] \
  [--max-cv-pct 20] \
  [--format markdown]
```

`performance-digest` reads run artifacts such as `resource-summary.json` and
`bench.json`, then renders a Markdown summary of host pressure, extension child
process usage, budget findings, benchmark memory peaks, baseline health, preview
metadata, browser-origin evidence, and missing-artifact gaps. It is a report
renderer only; it does not run benchmarks.

## Browser Evidence Compare

```sh
homeboy report browser-evidence-compare \
  --baseline-dir <DIR> \
  --candidate-dir <DIR> \
  --visual-compare \
  --visual-compare-provider <COMMAND> \
  --visual-artifacts-dir .homeboy/browser-visual-compare
```

`--visual-compare` pairs screenshot artifacts from matching scenario/profile/matrix
variants and delegates diff generation to the executable named by
`--visual-compare-provider`. The provider receives one input JSON path as its
final argument and emits normalized visual compare JSON on stdout. Homeboy
records mismatch metrics and artifact refs in the JSON report and Markdown output
without knowing which extension or tool produced the diff.

## Bench Coverage

```sh
homeboy report bench-coverage [component] [--path <checkout>] [--all] [--format markdown|json]
```

`bench-coverage` uses the existing `bench list`/`HOMEBOY_BENCH_LIST_ONLY=1`
contract, so it discovers scenarios without running benchmarks. The report maps
discovered scenarios onto generic hot command families such as `audit`, `bench`,
`lint`, `test`, `trace`, `refactor`, `runner`, and `offload`, then shows which
paths are covered or missing per component.

## Related

- [review](review.md)
- [issues](issues.md)
- [JSON output contract](../architecture/output-system.md)
