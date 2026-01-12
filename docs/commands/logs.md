# `homeboy logs`

## Synopsis

```sh
homeboy logs <COMMAND>
```

## Subcommands

- `list <projectId>`
- `show <projectId> <path> [-n|--lines <lines>] [-f|--follow]`
- `clear <projectId> <path>`

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy logs` returns a `LogsOutput` object as the `data` payload.

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

- [project](project.md)
- [file](file.md)
