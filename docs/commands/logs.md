# `homeboy logs`

## Synopsis

```sh
homeboy logs <COMMAND>
```

## Subcommands

- `list <project_id>`
- `show <project_id> <path> [-n|--lines <lines>] [-f|--follow]`
- `clear <project_id> <path>`
- `search <project_id> <path> <pattern> [options]`

### `search`

```sh
homeboy logs search <project_id> <path> <pattern> [options]
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

> Note: `logs list`, `logs show` (without `--follow`), `logs clear`, and `logs search` output JSON wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The object below refers to `data`.

- `command`: `logs.list` | `logs.show` | `logs.clear` | `logs.search`
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
- `evidence`: machine-readable metadata describing the captured log snippet

Pinned log objects (`pinned_logs.logs[]`) include the same fields as `log`, plus `label` when configured.

Search result object (`search_result`):

- `path`: full resolved path
- `pattern`: search pattern used
- `matches`: array of match objects
- `match_count`: number of matches
- `evidence`: machine-readable metadata describing the search scope and captured match output

Match objects (`matches[]`):

- `line_number`: line number in the file
- `content`: matching line content

Evidence metadata objects (`evidence`):

- `evidence_type`: `log_tail` or `log_search`
- `source_path`: full resolved log path
- `label`: pinned log label, when configured
- `requested_lines`: requested tail line count for `log_tail`
- `captured_lines`: number of lines captured in the returned snippet or grep output
- `byte_count`: byte count of the returned snippet or grep output
- `pattern`: search pattern for `log_search`
- `search_scope_lines`: `--lines` value for `log_search`, when provided
- `context_lines`: `--context` value for `log_search`, when provided
- `case_insensitive`: whether `--ignore-case` was used for `log_search`
- `match_count`: parsed match count for `log_search`

## Follow mode (`logs show --follow`)

`homeboy logs show --follow` uses an interactive SSH session (`tail -f`) and does not print the JSON envelope (it is treated as passthrough output).

## Exit code

- Follow mode exit code matches the underlying interactive command.

## Related

- [project](project.md)
- [file](file.md)
