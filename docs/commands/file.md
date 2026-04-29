# `homeboy file`

## Synopsis

```sh
homeboy file <COMMAND>
```

## Subcommands

- `list <project_id> <path>`
- `read <project_id> <path>`
- `write <project_id> <path>` (reads content from stdin)
- `delete <project_id> <path> [-r|--recursive]` (delete directories recursively)
- `rename <project_id> <old_path> <new_path>`
- `find <project_id> <path> [options]` (search for files by name)
- `grep <project_id> <path> <pattern> [options]` (search file contents)
- `download <project_id> <path> [local_path] [-r|--recursive]`
- `upload <server> <local_path> <remote_path> [-c|--compress] [--dry-run]`
- `copy <source> <destination> [-r|--recursive] [-c|--compress] [--dry-run] [--exclude <pattern>]`
- `sync <source> <destination> [-c|--compress] [--dry-run] [--exclude <pattern>]`

`copy` and `sync` targets use `local/path` or `server_id:/path` syntax. `sync` is recursive and non-deleting by default; it does not expose a delete mode.

### `find`

```sh
homeboy file find <project_id> <path> [options]
```

Options:

- `--name <pattern>`: Filename pattern (glob, e.g., `*.php`)
- `--type <f|d|l>`: File type: `f` (file), `d` (directory), `l` (symlink)
- `--max-depth <n>`: Maximum directory depth

Examples:

```sh
# Find all PHP files
homeboy file find mysite /var/www --name "*.php"

# Find directories named "cache"
homeboy file find mysite /var/www --name "cache" --type d

# Find files in top 2 levels only
homeboy file find mysite /var/www --name "*.log" --max-depth 2
```

### `grep`

```sh
homeboy file grep <project_id> <path> <pattern> [options]
```

Options:

- `--name <glob>`: Filter files by name pattern (e.g., `*.php`)
- `--max-depth <n>`: Maximum directory depth
- `-i, --ignore-case`: Case insensitive search

Examples:

```sh
# Find "TODO" in PHP files
homeboy file grep mysite /var/www "TODO" --name "*.php"

# Case-insensitive search
homeboy file grep mysite /var/www "error" -i

# Search with depth limit
homeboy file grep mysite /var/www "add_action" --name "*.php" --max-depth 3
```

### `upload`, `copy`, and `sync`

```sh
homeboy file upload prod ./report.json /tmp/report.json --dry-run
homeboy file copy ./dump.sql prod:/tmp/dump.sql --compress --dry-run
homeboy file copy prod:/tmp/dump.sql ./dump.sql --dry-run
homeboy file copy old:/var/www/uploads new:/var/www/uploads --recursive --exclude cache --dry-run
homeboy file sync ./uploads prod:/var/www/uploads --exclude cache --dry-run
```

Notes:

- `upload` is the ergonomic mirror of `download` for local-to-server uploads.
- `copy` preserves the old local↔remote and remote↔remote transfer target syntax.
- `sync` is directory-oriented and recursive, but does not delete files from the destination.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). `homeboy file` returns one of several output types as the `data` payload.

### Standard operations (list, read, write, delete, rename)

Fields:

- `command`: `file.list` | `file.read` | `file.write` | `file.delete` | `file.rename`
- `project_id`
- `base_path`: project base path if configured
- `path` / `old_path` / `new_path`: resolved full remote paths
- `recursive`: present for delete
- `entries`: for `list` (parsed from `ls -la`)
- `content`: for `read`
- `bytes_written`: for `write` (number of bytes written after stripping one trailing `\n` if present)
- `stdout`, `stderr`: included for error context when applicable
- `exit_code`, `success`

### Transfer output

`upload`, `copy`, and `sync` return the shared transfer payload:

- `source`
- `destination`
- `method`: `scp`, `cat-pipe`, or `tar-pipe`
- `direction`: `push`, `pull`, or `server-to-server`
- `recursive`
- `compress`
- `success`
- `error`
- `dry_run`

List entries (`entries[]`):

- `name`
- `path`
- `is_directory`
- `size`
- `permissions` (permission bits excluding the leading file type)

### Find output

Fields:

- `command`: `file.find`
- `project_id`
- `base_path`: project base path if configured
- `path`: search path
- `pattern`: name pattern if specified
- `matches`: array of matching file paths
- `match_count`: number of matches

### Grep output

Fields:

- `command`: `file.grep`
- `project_id`
- `base_path`: project base path if configured
- `path`: search path
- `pattern`: search pattern
- `matches`: array of match objects
- `match_count`: number of matches

Match objects (`matches[]`):

- `file`: file path
- `line`: line number
- `content`: matching line content

## Exit code

This command returns `0` on success; failures are returned as errors.

## Related

- [logs](logs.md)
- [project](project.md)
