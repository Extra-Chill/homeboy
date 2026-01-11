# `homeboy version`

## Synopsis

```sh
homeboy version <COMMAND>
```

## Subcommands

### `show`

```sh
homeboy version show <component_id>
```

### `bump`

```sh
homeboy version bump <component_id> <patch|minor|major>
```

## JSON output

`VersionOutput`:

- `command`: `version.show` | `version.bump`
- `componentId`
- `versionFile`
- `versionPattern`
- `fullPath`
- `version` (for `show`)
- `oldVersion`, `newVersion` (for `bump`)

## Exit code

- `show`: `0` on success; errors if the version cannot be parsed.
- `bump`: `0` on success.

## Version bump workflow

Use `homeboy version` for mechanical version bumping. Focus AI tokens on changelog generation.

### Process

1. Get current version: `homeboy version show <component>`
2. Analyze changes since last version using git diff and commits
3. Decide interval based on changes (generally patch for small changes)
4. Bump version: `homeboy version bump <component> patch|minor|major`
5. Update `changelog.md` with specific code-level changes
6. Commit: `homeboy git commit <component> "Bump version to X.Y.Z"`
7. Push: `homeboy git push <component>`
8. Build: `homeboy build <component>`

### Notes

- Components must have `version_file` and `build_command` configured
- Supported version formats: `.toml`, `.json`, `.php` (WordPress headers), or custom pattern
- Use `homeboy component show <component>` to check configuration
- Tagging is a separate release concern - only tag when explicitly instructed

## Related

- [build](build.md)
- [component](component.md)
- [git](git.md)
