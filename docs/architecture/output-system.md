# JSON output contract

Homeboy prints JSON to stdout for most commands.

Exceptions:

- `homeboy docs` prints raw markdown (or newline-delimited topic names for `homeboy docs list`).
- `homeboy changelog` (default show) prints raw markdown by default; in JSON mode it returns JSON with a `content` field containing the markdown.
- `homeboy review --report pr-comment` prints a raw markdown PR-comment report.
- `homeboy trace --report markdown` prints a raw markdown trace report.
- `homeboy runs compare` prints raw markdown unless `--output` is provided.
- `homeboy report failure-digest` prints raw markdown because markdown is currently the only report format.
- `homeboy list` prints clap help text (raw; not JSON-wrapped).
- `homeboy ssh` connect mode and `homeboy logs show --follow` use interactive passthrough output.

For raw markdown commands, `--output <path>` still writes a JSON file when the
command supports output artifacts. The generic output-file shape is the normal
success envelope with `data` set to the rendered markdown string. Commands with
special output artifacts keep their documented artifact contract: `review`
writes the stable review artifact, and `trace --json-summary` writes the trace
summary artifact.

## Top-level envelope

In JSON mode, Homeboy prints a `CliResponse<T>` where `T` is the
**command-specific output struct**.

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

## Full status report output

The `homeboy status --full` command returns actionable intelligence about the current context.

### Status section

The `status` object surfaces what needs attention:

```json
{
  "status": {
    "ready_to_deploy": ["component-a", "component-b"],
    "needs_release": ["component-c"],
    "has_uncommitted": ["component-d"],
    "config_gaps": 5
  }
}
```

Fields (all arrays/counts skip serialization when empty/zero):
- `ready_to_deploy`: Components with no uncommitted changes and no commits since version
- `needs_release`: Components with releasable code commits since the current version baseline
- `has_uncommitted`: Components with uncommitted changes in working directory
- `config_gaps`: Total count of configuration gaps across all components

### Summary section

The `summary` object provides counts for quick overview:

```json
{
  "summary": {
    "total_components": 24,
    "by_extension": { "wordpress": 21, "rust": 2, "swift": 1 },
    "by_status": { "clean": 5, "uncommitted": 8, "needs_release": 11 }
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
      "extension": "wordpress",
      "status": "needs_release",
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
    "11 components have unreleased commits. Release with `homeboy release <id>`."
  ]
}
```

## Error fields

`error` is a `CliError`.

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
| 4 | not found / missing state (`project.not_found`, `server.not_found`, `component.not_found`, `extension.not_found`, `project.no_active`) |
| 10 | SSH errors (`ssh.*`) |
| 20 | remote/deploy/git errors (`remote.*`, `deploy.*`, `git.*`) |

## Success payload

On success, `data` is the command-specific output struct (varies by command).

## Golden Output Fixtures

Automation-facing JSON surfaces should have golden output fixtures that serialize
typed command payloads through the same `CliResponse` envelope used by stdout and
`--output`.

Quality command fixtures live under `tests/fixtures/output_contracts/quality/`
and are enforced by `tests/output_contracts_test.rs`. The first required quality
set covers `audit`, `lint`, `test`, and `review`; adding or changing stable fields
in those payloads should update the matching fixture deliberately.

## Observation-backed payloads

`--output` remains the per-invocation command-result artifact. It must continue
to work for CI wrappers, scripts, and environments where the local observation
SQLite database is unavailable.

Observation-backed commands may add an optional `observation` field using the
`homeboy/observation-pointer/v1` shape:

```json
{
  "observation": {
    "schema": "homeboy/observation-pointer/v1",
    "run_id": "abc123",
    "kind": "review",
    "details": {
      "query": "homeboy runs show abc123",
      "artifacts": "homeboy runs artifacts abc123",
      "export_bundle": "homeboy runs export --run abc123 --output ~/.local/share/homeboy/exports/abc123"
    }
  }
}
```

Rules:

- The field is additive and optional; absence means the best-effort observation
  write was unavailable or the command is not observation-backed.
- Existing command payload fields stay intact for backward compatibility.
- Heavy evidence should live in observation records when available; command
  output should keep summary/counts/status and include exact drill-down commands.
- Export examples should point outside the source checkout. CI wrappers should
  stage observation bundles under runner temp storage before uploading them as
  the `homeboy-observations` artifact.
- Observation store failures must not fail an otherwise successful command.

## Command payload conventions

Many command outputs include a `command` string field:

- Values follow a dotted namespace (for example: `project.show`, `server.key.generate`).

## Captured Output

Commands that execute external processes include captured output in their response
when running in non-interactive mode.

The `CapturedOutput` primitive (`src/core/engine/command.rs`) provides:
- `stdout`: Captured standard output (omitted if empty)
- `stderr`: Captured standard error (omitted if empty)

Commands using this primitive:
- `extension run` (captured mode only)
- `lint`
- `test`
- `build`

## Related

- [CI result JSON contract](ci-results-contract.md)
- [Docs command JSON](../commands/docs.md)
- [Changelog command JSON](../commands/changelog.md)
