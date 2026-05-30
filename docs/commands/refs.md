# `homeboy refs`

Find references to a symbol or term in one component or a small set of components.

## Synopsis

```sh
homeboy refs <symbol> [--component <id>...] [--components <id,id>] [--path <path>]
homeboy refs <symbol> [--scope code|config|all] [--context key|variable|parameter|all]
```

## Options

- `--component <id>` — target a component by ID; repeat for multiple components.
- `--components <id,id>` — target multiple components with a comma-separated list.
- `--path <path>` — inspect a single checkout path directly.
- `--scope <scope>` — limit the search to `code`, `config`, or `all`.
- `--literal` — use exact string matching instead of boundary-aware variants.
- `--files <glob>` — include only files matching a glob; repeatable.
- `--exclude <glob>` — exclude files matching a glob; repeatable.
- `--context <context>` — filter by syntactic context: `key`, `variable`, `parameter`, or `all`.

## Examples

```sh
homeboy refs DependencyStackEdge --path /tmp/homeboy
homeboy refs package_name --component data-machine --component homeboy --scope code
homeboy refs old_config_key --components data-machine,homeboy --context key
```

The command exits with status `1` when no references are found, so scripts can treat an empty result as a failed lookup.

## Related

- [refactor](refactor.md)
- [component](component.md)
