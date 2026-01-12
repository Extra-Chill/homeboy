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

### Non-follow subcommands

> Note: `logs list`, `logs show` (without `--follow`), and `logs clear` output JSON wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below refers to `data.payload`.

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

## Follow mode (`logs show --follow`)

`homeboy logs show --follow` uses an interactive SSH session (`tail -f`) and does not print the JSON envelope (it is treated as passthrough output).

## Exit code

- Follow mode exit code matches the underlying interactive command.

## Related

- [project](project.md)
- [file](file.md)
