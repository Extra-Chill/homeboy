# `homeboy logs`

## Synopsis

```sh
homeboy logs <COMMAND>
```

## Subcommands

- `list <projectId>`
- `show <projectId> <path> [-n|--lines <lines>] [-f|--follow]`
- `clear <projectId> <path>`
- `search <projectId> <path> <pattern> [options]`

### `search`

```sh
homeboy logs search <projectId> <path> <pattern> [options]
```

Options:

- `-i, --ignore-case`: Case insensitive search
- `-n, --lines <n>`: Limit to last N lines before searching
- `-C, --context <n>`: Show N lines of context around matches

Examples:

```sh
# Search for errors in a log file
homeboy logs search mysite /var/log/php-errors.log "Fatal error"

# Case-insensitive search with context
homeboy logs search mysite /var/log/apache/error.log "timeout" -i -C 3

# Search last 1000 lines only
homeboy logs search mysite /var/log/debug.log "user_id" -n 1000
```

## JSON output

### Non-follow subcommands

Note: `logs show` accepts `--lines` even in follow mode, but it is ignored when `--follow` is set.

> Note: `logs list`, `logs show` (without `--follow`), `logs clear`, and `logs search` output JSON wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below refers to `data`.

- `command`: `logs.list` | `logs.show` | `logs.follow` | `logs.clear` | `logs.search`
- `project_id`
- `entries`: present for `list`
- `log`: present for `show` (non-follow)
- `cleared_path`: present for `clear`
- `search_result`: present for `search`

Entry objects (`entries[]`):

- `path`
- `label`
- `tail_lines`

Log object (`log`):

- `path` (full resolved path)
- `lines`
- `content` (tail output)

Search result object (`search_result`):

- `path`: full resolved path
- `pattern`: search pattern used
- `matches`: array of match objects
- `match_count`: number of matches

Match objects (`matches[]`):

- `line_number`: line number in the file
- `content`: matching line content

## Follow mode (`logs show --follow`)

`homeboy logs show --follow` uses an interactive SSH session (`tail -f`) and does not print the JSON envelope (it is treated as passthrough output).

## Exit code

- Follow mode exit code matches the underlying interactive command.

## Related

- [project](project.md)
- [file](file.md)
