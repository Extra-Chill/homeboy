# `homeboy version`

## Synopsis

```sh
homeboy version <COMMAND>
```

This command accepts the global flag `--dry-run` (see [Root command](../cli/homeboy-root-command.md)).

## Subcommands

### `show`

```sh
homeboy version show <componentId>
```

### `bump`

```sh
homeboy version bump <componentId> <patch|minor|major>
```

This command:

- Bumps all configured `versionTargets`.
- Finalizes the component changelog by moving the current "next" section (usually `Unreleased`) into a new `## <newVersion>` section.

Changelog entries must be added *before* running this command (recommended: `homeboy changelog add --json ...`).

Arguments:

- `<componentId>`: component ID
- `<patch|minor|major>`: version bump type

Dry-run mode:

```sh
homeboy --dry-run version bump <componentId> <patch|minor|major>
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
- `changelogPath` (resolved changelog path)
- `changelogFinalized` (always `true` on success)
- `changelogChanged` (whether the changelog file was modified)

Errors:

- `bump` errors if the changelog cannot be resolved, if the changelog is out of sync with the current version, or if the "next" section is missing/empty.

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
