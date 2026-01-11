# `homeboy logs`

## Synopsis

```sh
homeboy logs <COMMAND>
```

## Subcommands

- `list <project_id>`
- `show <project_id> <path> [-n <lines>] [--follow]`
- `clear <project_id> <path>`

## JSON output

`homeboy logs` returns a `LogsOutput` object.

- `command`: `logs.list` | `logs.show` | `logs.follow` | `logs.clear`
- `projectId`
- `entries`: present for `list`
- `log`: present for `show` (non-follow)
- `clearedPath`: present for `clear`

Entry objects (`entries[]`):

- `path`
- `label`
- `tailLines`

Log object (`log`):

- `path` (full resolved path)
- `lines`
- `content` (tail output)

## Exit code

- `logs.follow` uses an interactive SSH session; exit code matches the underlying process.

## Related

- [pin](pin.md)
- [file](file.md)
