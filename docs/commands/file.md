# `homeboy file`

## Synopsis

```sh
homeboy file <COMMAND>
```

## Subcommands

- `list <project_id> <path>`
- `read <project_id> <path>`
- `write <project_id> <path> [--apply]` (reads content from stdin)
- `mkdir <project_id> <path>` (create a directory)
- `delete <project_id> <path> [-r|--recursive] [--apply]` (delete files or directories)
- `rename <project_id> <old_path> <new_path>`
- `find <project_id> <path> [options]` (search for files by name)
- `grep <project_id> <path> <pattern> [options]` (search file contents)
- `download <project_id> <path> [local_path] [-r|--recursive]`
- `upload <server> <local_path> <remote_path> [-c|--compress] [--dry-run]`
- `copy <source> <destination> [-r|--recursive] [-c|--compress] [--dry-run] [--exclude <pattern>]`
- `sync <source> <destination> [-c|--compress] [--dry-run] [--exclude <pattern>]`
- `edit <project_id> <file_path> [operations] [-n|--dry-run] [-f|--force]`

`copy` and `sync` targets use `local/path` or `server_id:/path` syntax. `sync` is recursive and non-deleting by default; it does not expose a delete mode.

### `write` and `delete`

`write` and `delete` default to non-mutating plan output. Pass `--apply` to perform the remote mutation.

```sh
printf 'content' | homeboy file write mysite /tmp/example.txt
printf 'content' | homeboy file write mysite /tmp/example.txt --apply
homeboy file delete mysite /tmp/example.txt
homeboy file delete mysite /tmp/example.txt --apply
```

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

### `copy` and `sync`

```sh
homeboy file copy ./report.json prod:/tmp/report.json --dry-run
homeboy file copy ./dump.sql prod:/tmp/dump.sql --compress --dry-run
homeboy file copy prod:/tmp/dump.sql ./dump.sql --dry-run
homeboy file copy old:/var/www/uploads new:/var/www/uploads --recursive --exclude cache --dry-run
homeboy file sync ./uploads prod:/var/www/uploads --exclude cache --dry-run
```

Notes:

- `copy` preserves the old local↔remote and remote↔remote transfer target syntax.
- `file upload` is deprecated; use `file copy <local> <server>:<path>` for local-to-server uploads.
- `sync` is directory-oriented and recursive, but does not delete files from the destination.

### `edit`

```sh
homeboy file edit <project_id> <file_path> [operations]
```

Operations:

- `--replace-line <n> --replace-line-content <content>`: replace one line by number.
- `--insert-after <n> --insert-after-content <content>`: insert content after a line.
- `--insert-before <n> --insert-before-content <content>`: insert content before a line.
- `--delete-line <n>`: delete one line by number.
- `--delete-lines <start> <end>`: delete an inclusive line range.
- `--replace-pattern <pattern> --replace-pattern-content <content>`: replace a unique pattern match.
- `--replace-all-pattern <pattern> --replace-all-content <content>`: replace all pattern matches.
- `--delete-pattern <pattern>`: delete the line or content matched by a pattern operation.
- `--append <content>`: append content to the file.
- `--prepend <content>`: prepend content to the file.

Flags:

- `-n`, `--dry-run`: show changes without applying them.
- `-f`, `--force`: apply even when a pattern operation has multiple matches.

Examples:

```sh
homeboy file edit mysite /var/www/wp-config.php \
  --replace-pattern "WP_DEBUG', false" \
  --replace-pattern-content "WP_DEBUG', true" \
  --dry-run

homeboy file edit mysite /var/www/.maintenance --append "enabled"
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). `homeboy file` returns one of several output types as the `data` payload.

### Standard operations (list, read, write, mkdir, delete, rename)

Fields:

- `command`: `file.list` | `file.read` | `file.write` | `file.mkdir` | `file.delete` | `file.rename`
- `project_id`
- `base_path`: project base path if configured
- `path` / `old_path` / `new_path`: resolved full remote paths
- `recursive`: present for delete
- `entries`: for `list` (parsed from `ls -la`)
- `content`: for `read`
- `bytes_written`: for `write` (number of bytes written after stripping one trailing `\n` if present)
- `dry_run`, `action_required`: for guarded `write` and `delete` plans
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

### Edit output

Fields:

- `command`: `file.edit`
- `project_id`
- `base_path`: project base path if configured
- `path`: edited file path
- `changes_made`: array of line change records
- `change_count`: number of changes made or planned
- `success`
- `error`: error string when editing failed

## Exit code

This command returns `0` on success; failures are returned as errors.

## Related

- [logs](logs.md)
- [project](project.md)
