# JSON output contract

Homeboy prints JSON to stdout for most commands.

Exceptions:

- `homeboy docs` prints raw markdown content when `--list` is not used.
- `homeboy init` prints raw markdown.
- `homeboy changelog show` prints raw markdown.
- `homeboy list` prints clap help text (raw; not JSON-wrapped).
- `homeboy ssh` connect mode and `homeboy logs show --follow` use interactive passthrough output.

## Top-level envelope

In JSON mode, Homeboy prints a `CliResponse<T>` (implemented in the `homeboy-cli` crate) where `T` is the **command-specific output struct**.

Success:

```json
{
  "success": true,
  "data": { "...": "..." },
  "warnings": [
    {
      "code": "validation.invalid_argument",
      "message": "Human-readable message",
      "details": {},
      "hints": [{ "message": "..." }],
      "retryable": false
    }
  ]
}
```

Notes:

- The CLI emits command-scoped warnings at the **top level** (`warnings`).
- `data` is the serialized command output (not a `payload` wrapper).

Note: internally, the CLI uses an intermediate `CmdSuccess { payload, warnings }`, but it is converted into the final JSON envelope where `payload` becomes `data`.

Failure:

```json
{
  "success": false,
  "error": {
    "code": "internal.unexpected",
    "message": "Human-readable message",
    "details": {},
    "hints": [{ "message": "..." }],
    "retryable": false
  }
}
```

Notes:

- `data` is omitted on failure.
- `error` is omitted on success.
- `warnings` is omitted when empty.
- `error.hints`/`error.retryable` and `warnings[*].hints`/`warnings[*].retryable` are omitted when not set.

## Error fields

`error` is a `CliError` (implemented in the `homeboy-cli` crate).

## Warning fields

Each item in top-level `warnings` is a `CliWarning` (implemented in the `homeboy-cli` crate).

- `code` (string): stable error code (see `homeboy::error::ErrorCode::as_str()`).
- `message` (string): human-readable message.
- `details` (JSON value): structured error details (may be `{}`).
- `hints` (optional array): additional guidance.
- `retryable` (optional bool): when present, indicates whether retry may succeed.

## Exit codes

- Each subcommand returns `Result<(T, i32)>` where `T` is the success payload and `i32` is the intended process exit code.
- On success, the process exit code is the returned `i32`, clamped to `0..=255`.
- On error, Homeboy maps error codes to exit codes:

| Exit code | Meaning (by error code group) |
|---:|---|
| 1 | internal errors (`internal.*`) |
| 2 | config/validation errors (`config.*`, `validation.*`) |
| 4 | not found / missing state (`project.not_found`, `server.not_found`, `component.not_found`, `module.not_found`, `project.no_active`) |
| 10 | SSH errors (`ssh.*`) |
| 20 | remote/deploy/git errors (`remote.*`, `deploy.*`, `git.*`) |

## Success payload

On success, `data` is the command-specific output struct (varies by command).

Warnings are emitted at the top level (`warnings`).

## Command payload conventions

Many command outputs include a `command` string field:

- Values follow a dotted namespace (for example: `project.show`, `server.key.generate`).

## Related

- [Docs command JSON](../commands/docs.md)
- [Changelog command JSON](../commands/changelog.md)

