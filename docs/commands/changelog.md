# `homeboy changelog`

## Synopsis

```sh
homeboy changelog [COMMAND]
```

## Description

`homeboy changelog` prints the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`) as raw markdown.

## Subcommands


```sh
homeboy changelog
```

Shows the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`).

This prints raw markdown to stdout.

### `add`

```sh
homeboy changelog add <componentId> <message> [--project-id <projectId>]
homeboy --json <spec> changelog add
```

Notes:

- In non-JSON mode, `add` accepts *positional* `componentId` and `message`.
- When `--json` is provided, positional args are ignored and the payload's `messages` array is applied in order.

Adds one or more changelog items to the configured "next" section in the component's changelog file.

`--json` is global, but not every command supports JSON input. (For example, `homeboy version` rejects `--json`.)

Configuration / defaults:

- Changelog path resolution:
  - If `changelogTargets` is set in the component config, the first target's `file` is used (relative to `component.localPath` unless it's absolute).
  - Otherwise, Homeboy auto-detects (in order): `CHANGELOG.md`, then `docs/changelog.md`.
  - If neither exists, the command errors and asks you to create a changelog file or set `component.changelogTargets[0].file`.
  - If both exist, the command errors and asks you to set `component.changelogTargets[0].file` to disambiguate.
- "Next section" resolution:
  - If no label is configured, Homeboy defaults to `Unreleased`.
  - If no aliases are configured, Homeboy matches both `Unreleased` and `[Unreleased]`.
  - Config overrides (most specific first): component config → project config → global `config.json`.


## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

`homeboy changelog` returns a tagged union:

- `command`: `show` (default) | `add`

### JSON output (default)

This section applies only when JSON output is used.

```json
{
  "command": "show",
  "topicLabel": "changelog",
  "content": "<markdown content>"
}
```

### JSON output (add)

```json
{
  "command": "add",
  "componentId": "<componentId>",
  "projectId": "<projectId>|null",
  "changelogPath": "<absolute/or/resolved/path.md>",
  "nextSectionLabel": "<label>",
  "messages": ["<message>", "<message>"],
  "itemsAdded": 2,
  "changed": true
}
```

## Errors

- `show`: errors if embedded docs do not contain `changelog`
- `add`: errors if changelog path cannot be resolved, or if `messages` is empty / contains empty strings

## Related

- [Docs command](docs.md)
- [Changelog content](../changelog.md)
