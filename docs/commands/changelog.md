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
homeboy changelog add <component_id> <message> [--project-id <id>]
```

Adds a changelog item to the configured "next" section in the component's changelog file.
Configuration / defaults:

- Changelog path resolution:
  - If `changelogTargets` is set in the component config, the first target is used.
  - Otherwise, Homeboy auto-detects (in order): `CHANGELOG.md`, then `docs/changelog.md`.
  - If neither exists (or both exist), the command errors and asks you to set `changelogTargets`.
- "Next section" resolution:
  - If no label is configured, Homeboy defaults to `Unreleased`.
  - If no aliases are configured, Homeboy matches both `Unreleased` and `[Unreleased]`.
  - Config overrides (most specific first): `component.json` → `project.json` → `config.json`.


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
  "componentId": "<component_id>",
  "projectId": "<id>|null",
  "changelogPath": "</absolute/or/resolved/path.md>",
  "nextSectionLabel": "<label>",
  "message": "<message>",
  "changed": true
}
```

## Errors

- `show`: errors if embedded docs do not contain `changelog`
- `add`: errors if `changelogTargets` or the next-section label is not configured

## Related

- [Docs command](docs.md)
- [Changelog content](../changelog.md)
