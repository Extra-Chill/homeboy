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
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy version` returns a `VersionOutput` object as the `data` payload.

`VersionOutput`:

- `command`: `version.show` | `version.bump`
- `componentId`
- `version` (for `show`)
- `oldVersion`, `newVersion` (for `bump`)
- `targets`: array of `{ versionFile, versionPattern, fullPath, matchCount }`
- `changelogPath` (when `--changelog-add` is used and a changelog is available)
- `changelogItemsAdded` (when `--changelog-add` is used)
- `changelogFinalized` (when `--changelog-add` is used and a changelog is available)
- `changelogChanged` (when any changelog update occurs)
- Global `warnings` may be present (for example, when `--changelog-add` is used but no changelog can be resolved)

## Exit code

- `show`: `0` on success; errors if the version cannot be parsed.
- `bump`: `0` on success.

## Notes

- Components must have `version_file` configured.
- `version_pattern` is optional; when omitted, a default pattern is selected based on the configured `version_file` name.

## Related

- [build](build.md)
- [component](component.md)
- [git](git.md)
