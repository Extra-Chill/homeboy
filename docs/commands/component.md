# `homeboy component`

## Synopsis

```sh
homeboy component <COMMAND>
```

## Subcommands

- `create <id> --name <name> --local-path <path> --remote-path <path> --build-artifact <path> [--version-file <path>] [--version-pattern <regex>] [--build-command <cmd>] [--is-network]`
- `import <json> [--skip-existing]`
- `show <id>`
- `set <id> [--name <name>] [--local-path <path>] [--remote-path <path>] [--build-artifact <path>] [--version-file <path>] [--version-pattern <regex>] [--build-command <cmd>] [--is-network] [--not-network]`
- `delete <id> [--force]`
- `list`

## JSON output

`ComponentOutput`:

- `action`: `create` | `import` | `show` | `set` | `delete` | `list`
- `componentId` (present for single-component operations)
- `success`
- `updatedFields`: list of field names updated by `set`
- `created`, `skipped`, `errors`: import status lists
- `component`: present for `create`, `show`, `set`
- `components`: present for `list`

## Exit code

- `import` returns exit code `1` if any errors occur while importing.

## Related

- [deploy](deploy.md)
- [build](build.md)
- [version](version.md)
