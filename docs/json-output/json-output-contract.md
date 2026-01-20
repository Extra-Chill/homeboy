# JSON output contract

Homeboy prints JSON to stdout for most commands.

Exceptions:

- `homeboy docs` prints raw markdown (or newline-delimited topic names for `homeboy docs list`).
- `homeboy changelog` (default show) prints raw markdown by default; in JSON mode it returns JSON with a `content` field containing the markdown.
- `homeboy list` prints clap help text (raw; not JSON-wrapped).
- `homeboy ssh` connect mode and `homeboy logs show --follow` use interactive passthrough output.

## Top-level envelope

In JSON mode, Homeboy prints a `CliResponse<T>` (implemented in the `homeboy-cli` crate) where `T` is the **command-specific output struct**.

Success:

```json
{
  "success": true,
  "data": { "...": "..." }
}
```

Failure:

```json
{
  "success": false,
  "error": {
    "code": "internal.unexpected",
    "message": "Human-readable message",
    "details": {}
  }
}
```

Notes:

- `data` is omitted on failure.
- `error` is omitted on success.
- `error.hints`/`error.retryable` are omitted when not set.
- JSON serialization errors return `internal.json_error` (no silent fallback).

## Init command output

The `homeboy init` command returns actionable intelligence about the current context.

### Status section

The `status` object surfaces what needs attention:

```json
{
  "status": {
    "ready_to_deploy": ["component-a", "component-b"],
    "needs_version_bump": ["component-c"],
    "has_uncommitted": ["component-d"],
    "config_gaps": 5
  }
}
```

Fields (all arrays/counts skip serialization when empty/zero):
- `ready_to_deploy`: Components with no uncommitted changes and no commits since version
- `needs_version_bump`: Components with commits since last version bump
- `has_uncommitted`: Components with uncommitted changes in working directory
- `config_gaps`: Total count of configuration gaps across all components

### Summary section

The `summary` object provides counts for quick overview:

```json
{
  "summary": {
    "total_components": 24,
    "by_module": { "wordpress": 21, "rust": 2, "swift": 1 },
    "by_status": { "clean": 5, "uncommitted": 8, "needs_bump": 11 }
  }
}
```

### Components section

Components are returned in compact `ComponentSummary` format:

```json
{
  "components": [
    {
      "id": "extra-chill-blog",
      "path": "extrachill-plugins/extrachill-blog",
      "module": "wordpress",
      "status": "needs_bump",
      "commits_since_version": 2
    }
  ]
}
```

For full component details, use `homeboy component show <id>`.

### Next steps

The `next_steps` array contains context-aware actionable guidance based on the current status:

```json
{
  "next_steps": [
    "8 components have uncommitted changes. Review with `homeboy changes <id>`.",
    "11 components have unreleased commits. Bump with `homeboy version bump <id>`."
  ]
}
```

## Error fields

`error` is a `CliError` (implemented in the `homeboy-cli` crate).

- `code` (string): stable error code (see `homeboy::error::ErrorCode::as_str()`).
- `message` (string): human-readable message.
- `details` (JSON value): structured error details (may be `{}`).
- `hints` (optional array): additional guidance.
- `retryable` (optional bool): when present, indicates whether retry may succeed.

## Exit codes

- Each subcommand returns `Result<(T, i32)>` where `T` is the success payload and `i32` is the intended process exit code.
- On success, the process exit code is the returned `i32`.
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

## Command payload conventions

Many command outputs include a `command` string field:

- Values follow a dotted namespace (for example: `project.show`, `server.key.generate`).

## Related

- [Docs command JSON](../commands/docs.md)
- [Changelog command JSON](../commands/changelog.md)

