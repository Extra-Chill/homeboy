# `homeboy version`

## Synopsis

```sh
homeboy version <COMMAND>
```

## Subcommands

### `show`

```sh
homeboy version show [<componentId>]
homeboy version show --cwd
```

### `bump`

```sh
homeboy version bump [<componentId>] <patch|minor|major>
homeboy version bump --cwd <patch|minor|major>
```

### `set`

```sh
homeboy version set [<componentId>] <newVersion>
```

`set` writes the version targets directly without incrementing and does not finalize the changelog.

### CWD Mode (--cwd)

Both subcommands support `--cwd` for ad-hoc operations in any directory without requiring component registration. When using `--cwd`, Homeboy auto-detects version files by checking for:

1. `Cargo.toml` (Rust)
2. `package.json` (Node.js)
3. `composer.json` (PHP)
4. `style.css` (WordPress themes)
5. `*.php` with WordPress plugin/theme header

This command:

- Bumps all configured `versionTargets`.
- Finalizes the component changelog by moving the current "next" section (usually `Unreleased`) into a new `## <newVersion>` section.

Changelog entries must be added *before* running this command (recommended: `homeboy changelog add --json ...`).

Recommended release workflow (non-enforced):

- Land work as scoped feature/fix commits first.
- Use `homeboy changes <componentId>` to review everything since the last tag.
- Add changelog items as user-facing release notes that capture anything impacting user or developer experience (not a copy of commit subjects).
- Run `homeboy version bump ...` when the only remaining local changes are release metadata (changelog + version).

In this version of Homeboy, the `--json` flag is on `changelog add` (not on `changelog`).

Arguments:

- `<componentId>`: component ID
- `<patch|minor|major>`: version bump type

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
- `oldVersion` (version before bump)
- `newVersion` (version after bump)
- `targets`: array of `{ file, pattern, fullPath, matchCount }`
- `changelogPath` (resolved changelog path)
- `changelogFinalized` (always `true` on success)
- `changelogChanged` (whether the changelog file was modified)

`homeboy version set` data payload:

- `command`: `version.set`
- `componentId`
- `oldVersion`
- `newVersion`
- `targets`: array of `{ file, pattern, fullPath, matchCount }`

Errors:

- `bump` errors if the changelog cannot be resolved, if the changelog is out of sync with the current version, or if the "next" section is missing/empty.

## Exit code

- `show`: `0` on success; errors if the version cannot be parsed.
- `bump`: `0` on success.
- `set`: `0` on success.

## Notes

- Components must have `versionTargets` configured (non-empty). Homeboy uses the first target as the primary version source.
- Each `versionTargets[]` entry has `file` and optional `pattern`. When `pattern` is omitted, a default pattern is selected based on the `file` name.

## Related

- [build](build.md)
- [component](component.md)
- [git](git.md)
