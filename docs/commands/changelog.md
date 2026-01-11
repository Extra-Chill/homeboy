# `homeboy changelog`

## Synopsis

```sh
homeboy changelog <COMMAND>
```

## Subcommands

### `show`

```sh
homeboy changelog show
```

Shows the embedded Homeboy CLI changelog documentation (from `docs/changelog.md`).

### `add`

```sh
homeboy changelog add <componentId> <message> [--project-id <projectId>]
homeboy changelog add --json <spec>
# Note: `--json` is a global flag but currently only supported for `homeboy changelog`.
```

Adds one or more changelog items to the configured "next" section in the component's changelog file.

In JSON mode (`--json`), positional args are not used. The payload's `messages` array is applied in order.
Configuration / defaults:

- Changelog path resolution:
  - If `changelogTargets` is set in the component config, the first target is used.
  - Otherwise, Homeboy auto-detects (in order): `CHANGELOG.md`, then `docs/changelog.md`.
  - If neither exists (or both exist), the command errors and asks you to set `changelogTargets`.
- "Next section" resolution:
  - If no label is configured, Homeboy defaults to `Unreleased`.
  - If no aliases are configured, Homeboy matches both `Unreleased` and `[Unreleased]`.
  - Config overrides (most specific first): component config → project config → global `config.json`.


## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

`homeboy changelog` returns a tagged union:

- `command`: `show` | `add`

### JSON output (show)

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
  "projectId": "<id>|null",
  "changelogPath": "</absolute/or/resolved/path.md>",
  "nextSectionLabel": "<label>",
  "messages": ["<message>", "<message>"],
  "itemsAdded": 2,
  "changed": true
}
```

## Errors

- `show`: errors if embedded docs do not contain `changelog`
- `add`: errors if changelog path cannot be resolved

## Related

- [Docs command](docs.md)
- [Changelog content](../changelog.md)
