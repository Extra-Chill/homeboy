# `homeboy component`

Manage standalone component configurations.

## Synopsis

```sh
homeboy component [OPTIONS] <COMMAND>
```


## Subcommands

### `create`

```sh
homeboy component create [OPTIONS] <NAME>
```

Options:

- `--json <spec>`: JSON input spec for create/update (supports single or bulk)
- `--skip-existing`: skip items that already exist (JSON mode only)
- `--local-path <path>`: absolute path to local source directory (required in CLI mode; `~` is expanded)
- `--remote-path <path>`: remote path relative to project `basePath` (required in CLI mode)
- `--build-artifact <path>`: build artifact path relative to `localPath` (required in CLI mode)
- `--version-target <TARGET>`: version target in format `file` or `file::pattern` (repeatable)
- `--build-command <command>`: build command to run in `localPath`
- `--extract-command <command>`: command to run after upload (optional)

### `show`

```sh
homeboy component show <id>
```

### `set`

```sh
homeboy component set <id> [OPTIONS]
```

Options:

- `--name <name>`: update display name
- `--local-path <path>`: update local path
- `--remote-path <path>`: update remote path
- `--build-artifact <path>`: update build artifact path
- `--version-target <TARGET>`: replace version targets (repeatable `file` or `file::pattern`)
- `--build-command <command>`: update build command
- `--extract-command <command>`: update extract command

### `delete`

```sh
homeboy component delete <id> [--force]
```

If the component is referenced by one or more projects, `--force` is required.

### `list`

```sh
homeboy component list
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md). The object below is the `data` payload.

`homeboy component` returns a `ComponentOutput` object.

```json
{
  "action": "create|show|set|delete|list|component.create",
  "componentId": "<id>|null",
  "success": true,
  "updatedFields": ["name", "localPath"],
  "component": { },
  "components": [ ],
  "import": null
}
```

Notes:

- `action` is `component.create` only for JSON import mode (`homeboy component create --json ...`).
- Other subcommands use `create`, `show`, `set`, `delete`, or `list`.


## Related

- [build](build.md)
- [deploy](deploy.md)
- [project](project.md)
- [JSON output contract](../json-output/json-output-contract.md)
