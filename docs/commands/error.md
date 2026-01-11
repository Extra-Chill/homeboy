# `homeboy error`

## Synopsis

```sh
homeboy error <COMMAND>
```

## Description

Developer-facing error registry commands.

`homeboy error` lists and explains Homeboy error codes and is intended for troubleshooting and for building tools that react to stable, namespaced error codes.

## Subcommands

### `codes`

```sh
homeboy error codes
```

List all available error codes.

### `explain <code>`

```sh
homeboy error explain <code>
```

Explain a specific error code.

Arguments:

- `<code>`: error code string (example: `ssh.auth_failed`)

## JSON output

Homeboy wraps command output in the global JSON envelope described in the [JSON output contract](../json-output/json-output-contract.md).

The `data` payload is one of:

### JSON output (codes)

```json
{
  "command": "error.codes",
  "codes": [
    {
      "code": "ssh.auth_failed",
      "summary": "SSH authentication failed"
    }
  ]
}
```

### JSON output (explain)

```json
{
  "command": "error.explain",
  "help": {
    "code": "remote.command_failed",
    "summary": "Remote command failed",
    "detailsSchema": {
      "command": "string",
      "exitCode": "number",
      "stdout": "string",
      "stderr": "string",
      "target": "object"
    },
    "hints": [{ "message": "Inspect stdout/stderr in error.details for the underlying failure" }]
  }
}
```

## Errors

- `validation.unknown_error_code`: returned when `<code>` does not match a known Homeboy error code.

## Related

- [Commands index](commands-index.md)
- [JSON output contract](../json-output/json-output-contract.md)
- [Docs command](docs.md)
