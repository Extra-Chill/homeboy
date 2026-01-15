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

- Bumps all configured `version_targets` using semantic versioning (X.Y.Z).
- Finalizes the component changelog by moving the current "next" section (usually `Unreleased`) into a new `## [<newVersion>] - YYYY-MM-DD` section.

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
- `component_id`
- `version` (detected current version)
- `targets`: array of `{ file, pattern, full_path, match_count }`

`homeboy version bump` data payload:

- `command`: `version.bump`
- `component_id`
- `old_version` (version before bump)
- `new_version` (version after bump)
- `targets`: array of `{ file, pattern, full_path, match_count }`
- `changelog_path` (resolved changelog path)
- `changelog_finalized` (always `true` on success)
- `changelog_changed` (whether the changelog file was modified)

`homeboy version set` data payload:

- `command`: `version.set`
- `component_id`
- `old_version`
- `new_version`
- `targets`: array of `{ file, pattern, full_path, match_count }`

Errors:

- `bump` errors if the changelog cannot be resolved, if the changelog is out of sync with the current version, or if the "next" section is missing/empty.
- `bump` errors if the current version is not semantic versioning format (X.Y.Z).

Notes:

- Homeboy does not auto-fix existing changelogs. If the next section is missing or empty, follow the hints in the error to fix it manually.

## Exit code

- `show`: `0` on success; errors if the version cannot be parsed.
- `bump`: `0` on success.
- `set`: `0` on success.

## Notes

- Components must have `version_targets` configured (non-empty). Homeboy uses the first target as the primary version source.
- Each `version_targets[]` entry has `file` and optional `pattern`. When `pattern` is omitted, a default pattern is selected based on the `file` name.

## Related

- [build](build.md)
- [component](component.md)
- [git](git.md)
