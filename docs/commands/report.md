# `homeboy report`

Render reports from Homeboy structured output artifacts.

## Synopsis

```sh
homeboy report <COMMAND>
```

## Subcommands

- `failure-digest` — render a markdown failure digest from Homeboy command output JSON files
- `bench-coverage` — report list-only benchmark coverage for hot command paths

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
