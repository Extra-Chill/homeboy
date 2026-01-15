# `homeboy file`

## Synopsis

```sh
homeboy file <COMMAND>
```

## Subcommands

- `list <projectId> <path>`
- `read <projectId> <path>`
- `write <projectId> <path>` (reads content from stdin)
- `delete <projectId> <path> [-r|--recursive]` (delete directories recursively)
- `rename <projectId> <oldPath> <newPath>`
- `find <projectId> <path> [options]` (search for files by name)
- `grep <projectId> <path> <pattern> [options]` (search file contents)

### `find`

```sh
homeboy file find <projectId> <path> [options]
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
homeboy file grep <projectId> <path> <pattern> [options]
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

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). `homeboy file` returns one of several output types as the `data` payload.

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
