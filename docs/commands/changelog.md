# `homeboy changelog`

## Synopsis

```sh
homeboy changelog [COMMAND]
```

## Description

`homeboy changelog` prints the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`) as raw markdown by default.

In JSON output mode, the default `show` output is returned as JSON (with a `content` field containing the markdown).

> **Note:** Homeboy generates changelog entries automatically from conventional-prefixed commits (`feat:` / `fix:` / etc.) at release time. There is no `changelog add` command — users don't hand-curate changelog bullets. See `homeboy release` and the commits since the last tag.

## Subcommands

### Default (show)

```sh
homeboy changelog
homeboy changelog --self
homeboy changelog show
homeboy changelog show <component_id>
```

Shows the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`), or a specific component's changelog when a component ID is provided.

Options:

- `--self`: Show Homeboy's own changelog (release notes) instead of a component's changelog

This prints raw markdown to stdout.

## Prerequisites

Configure the changelog path:

```sh
homeboy component set <id> --changelog-target "CHANGELOG.md"
```

This is required for `release`.

Release can bootstrap a missing configured changelog target on the first release. Changelog setup is owned by component configuration; there is no manual changelog initialization command.

## Changelog Resolution

Homeboy resolves the changelog from the component's `changelog_target` configuration for `show` (when a component ID is given) and for release-time finalization.

Configuration / defaults (strict by default):

- Changelog path resolution:
  - If `changelog_target` is set in the component config, that path is used (relative to `component.local_path` unless it's absolute).
  - If `changelog_target` is not configured, the command errors with instructions to set it.
- "Next section" resolution:
  - If no label is configured, Homeboy defaults to `Unreleased`.
  - If no aliases are configured, Homeboy matches both `Unreleased` and `[Unreleased]`.
  - If aliases are configured, Homeboy ensures the label and bracketed label are included for matching.
  - Config overrides (most specific first): component config → project config → defaults.

Notes:

- Homeboy does not auto-fix existing changelogs. If the next section is missing or malformed, commands will error with hints to fix it manually.


## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below is the `data` payload.

`homeboy changelog` returns a tagged union:

- `command`: `show` (default)

### JSON output (default / show)

This section applies only when JSON output is used.

```json
{
  "command": "show",
  "topic_label": "changelog",
  "content": "<markdown content>"
}
```

## Errors

- `show`: errors if embedded docs do not contain `changelog`, or if the component's changelog path cannot be resolved (when a component ID is provided)

## Related

- [Release command](release.md) — owns changelog entry generation from commits
- [Docs command](docs.md)
- [Changelog content](../changelog.md) — homeboy's own historical changelog
