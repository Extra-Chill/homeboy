# `homeboy changelog`

## Synopsis

```sh
homeboy changelog [COMMAND]
```

## Description

`homeboy changelog` prints the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`) as raw markdown by default.

In JSON output mode, the default `show` output is returned as JSON (with a `content` field containing the markdown).

## Subcommands


```sh
homeboy changelog
```

Shows the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`).

This prints raw markdown to stdout.

### `add`

```sh
homeboy changelog add <componentId> -m <message>
homeboy changelog add --cwd -m <message>
homeboy changelog add --json <spec>
```

Notes:

- Use `-m` or `--message` to provide the changelog entry.
- Changelog messages are intended to be user-facing release notes (capture anything impacting user or developer experience), not a 1:1 copy of commit subjects.
- When `--cwd` is used, Homeboy auto-detects the changelog file (see CWD Mode below).
- When `--json` is provided, other args are ignored and the payload's `messages` array is applied in order.

### `init`

```sh
homeboy changelog init <componentId>
homeboy changelog init <componentId> --path "docs/CHANGELOG.md"
homeboy changelog init <componentId> --configure
homeboy changelog init --cwd
```

Creates a new changelog file with the Keep a Changelog format (`## [X.Y.Z] - YYYY-MM-DD`).

Options:

- `--path <path>`: Custom path for changelog file (relative to component/cwd). Default: `CHANGELOG.md`
- `--configure`: Also update component config to add `changelog_target`
- `--cwd`: Use current working directory instead of a component

Requirements:

- Component must have `version_targets` configured (to determine initial version)
- Errors if changelog file already exists at target path

### CWD Mode (--cwd)

Both `add` and `init` subcommands support `--cwd` for ad-hoc operations in any directory without requiring component registration.

For `add`, Homeboy auto-detects the changelog file by checking for (in order):

1. `CHANGELOG.md`
2. `docs/changelog.md`
3. `HISTORY.md`
4. `changelog.md`

Adds one or more changelog items to the configured "next" section in the component's changelog file.

`--json` for this command is an `add` subcommand option (not a root/global flag).

Configuration / defaults (strict by default):

- Changelog path resolution:
  - If `changelog_target` is set in the component config, that path is used (relative to `component.local_path` unless it's absolute).
  - Otherwise, Homeboy auto-detects (in order): `CHANGELOG.md`, then `docs/changelog.md`.
  - If neither exists, the command errors and asks you to create a changelog file or set `component.changelog_target`.
  - If both exist, the command errors and asks you to set `component.changelog_target` to disambiguate.
- "Next section" resolution:
  - If no label is configured, Homeboy defaults to `Unreleased`.
  - If no aliases are configured, Homeboy matches both `Unreleased` and `[Unreleased]`.
  - If aliases are configured, Homeboy ensures the label and bracketed label are included for matching.
  - Config overrides (most specific first): component config → project config → defaults.

Notes:

- Homeboy does not auto-fix existing changelogs. If the next section is missing or empty, commands will error with hints to fix it manually.


## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

`homeboy changelog` returns a tagged union:

- `command`: `show` (default) | `add` | `init`

### JSON output (default)

This section applies only when JSON output is used.

```json
{
  "command": "show",
  "topic_label": "changelog",
  "content": "<markdown content>"
}
```

### JSON output (add)

```json
{
  "command": "add",
  "component_id": "<componentId>",
  "changelog_path": "<absolute/or/resolved/path.md>",
  "next_section_label": "<label>",
  "messages": ["<message>", "<message>"],
  "items_added": 2,
  "changed": true
}
```

### JSON output (init)

```json
{
  "command": "init",
  "component_id": "<componentId>",
  "changelog_path": "<absolute/path/to/CHANGELOG.md>",
  "initial_version": "0.3.2",
  "next_section_label": "Unreleased",
  "created": true,
  "configured": false
}
```

## Errors

- `show`: errors if embedded docs do not contain `changelog`
- `add`: errors if changelog path cannot be resolved, or if `messages` is empty / contains empty strings
- `init`: errors if changelog already exists, if component not found, or if no version targets configured

## Related

- [Docs command](docs.md)
- [Changelog content](../changelog.md)
