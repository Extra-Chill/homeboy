# `homeboy version`

## Synopsis

```sh
homeboy version <COMMAND>
```

## Subcommands

### `show`

```sh
homeboy version show <componentId>
```

### `bump`

```sh
homeboy version bump <componentId> <patch|minor|major> \
  [--changelog-add "<message>"]... \
  [--project-id <projectId>]

Dry-run mode:

```sh
homeboy --dry-run version bump <componentId> <patch|minor|major> \
  --changelog-add "<message>"
```
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy version` returns a `VersionOutput` object as the `data` payload.

`homeboy version show` data payload:

- `command`: `version.show`
- `componentId`
- `version` (detected current version)
- `targets`: array of `{ file, pattern, fullPath, matchCount }`

`homeboy version bump` data payload:

- `command`: `version.bump`
- `componentId`
- `version` (detected current version before bump)
- `newVersion` (version after bump)
- `targets`: array of `{ file, pattern, fullPath, matchCount }`
- `changelogPath` (when `--changelog-add` is used and a changelog is available)
- `changelogItemsAdded` (when `--changelog-add` is used)
- `changelogFinalized` (when `--changelog-add` is used and a changelog is available)
- `changelogChanged` (when any changelog update occurs)
- Global `warnings` may be present (for example, when `--changelog-add` is used but no changelog can be resolved)

## Exit code

- `show`: `0` on success; errors if the version cannot be parsed.
- `bump`: `0` on success.

## Notes

- Components must have `versionTargets` configured (non-empty). Homeboy uses the first target as the primary version source.
- Each `versionTargets[]` entry has `file` and optional `pattern`. When `pattern` is omitted, a default pattern is selected based on the `file` name.

## Related

- [build](build.md)
- [component](component.md)
- [git](git.md)
