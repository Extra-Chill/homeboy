# `homeboy pin`

## Synopsis

```sh
homeboy pin <COMMAND>
```

## Subcommands

- `list <project_id> --type <file|log>`
- `add <project_id> <path> --type <file|log> [--label <label>] [--tail <lines>]`
- `remove <project_id> <path> --type <file|log>`

## JSON output

`PinOutput`:

- `command`: `pin.list` | `pin.add` | `pin.remove`
- `projectId`
- `type`: `file` | `log`
- `items`: present for `list`
- `added`: present for `add`
- `removed`: present for `remove`

List item (`items[]`):

- `path`
- `label`
- `displayName`
- `tailLines` (logs only)

Change object (`added`/`removed`):

- `path`
- `type`

## Related

- [file](file.md)
- [logs](logs.md)
