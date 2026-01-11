# JSON output contract

Homeboy prints a JSON value for both success and error results.

## Where JSON comes from

- Each subcommand returns `(T, i32)` where `T` is a serializable payload type and `i32` is the intended process exit code.
- `homeboy` serializes `T` into a `serde_json::Value`.
- A unified printer prints either the serialized value or an error.

## Exit code

- Success and failure both return a process exit code.
- For commands returning `(T, i32)`, the process exit code is the returned `i32`, clamped to `0..=255`.
- For errors, the process exit code is `1`.

## Success payload

On success, the JSON payload is the command’s output struct (varies by command).

## Error payload

On error, the output is an error JSON object printed by the shared output formatter.

The precise error JSON shape is defined by `homeboy_core::output::print_result`.

## Command payload conventions

Many commands include a string field that identifies the action taken:

- Common fields: `command`, `action`
- Values often follow a dotted namespace (e.g. `project.show`, `server.key.generate`).

## Related

- Embedded docs outputs: [Docs command JSON](../commands/docs.md)
- Changelog output: [Changelog command JSON](../commands/changelog.md)
