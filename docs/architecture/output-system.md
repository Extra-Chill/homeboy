# JSON output contract

Homeboy prints JSON to stdout for most commands.

Exceptions:

- `homeboy docs` prints raw markdown (or newline-delimited topic names for `homeboy docs list`).
- `homeboy changelog` (default show) prints raw markdown by default; in JSON mode it returns JSON with a `content` field containing the markdown.
- `homeboy review --report pr-comment` prints a raw markdown PR-comment report.
- `homeboy trace --report markdown` prints a raw markdown trace report.
- `homeboy runs compare` prints raw markdown unless `--output` is provided.
- `homeboy report failure-digest` prints raw markdown because markdown is currently the only report format.
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
- `ready_to_deploy`: Components in a clean release state — no uncommitted changes
  and no commits since the last version tag. **Git/workspace state only**: it
  means "has a release tag that *could* be deployed", NOT "the deploy target is
  behind the latest release". For a target-accurate diff, run
  `homeboy status <project>` and inspect the `outdated` components. See #4588.
- `ready_to_deploy_note`: Present only when `ready_to_deploy` is non-empty.
  A clarifying string warning that the list is git-state-only and pointing at
  `homeboy status <project>` for the target-accurate deploy diff.
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
| 5 | release skipped — no tag/package/GitHub Release produced (`release` when the plan reports `status: "skipped"`; the data payload still carries `skipped_reason` + an actionable force hint) |
| 10 | SSH errors (`ssh.*`) |
| 20 | remote/deploy/git errors (`remote.*`, `deploy.*`, `git.*`) |

Note: `0` means success and `3` means a release completed with post-release warnings. A non-zero exit code makes the JSON envelope report `success: false`, even when `data` is present (e.g. a skipped release returns its full result payload alongside `success: false`).

## Success payload

On success, `data` is the command-specific output struct (varies by command).

## Output Contract Coverage

Automation-facing JSON surfaces should keep behavior-level coverage for routing,
envelope semantics, and public variant discrimination. Prefer command execution or
focused contract assertions over golden fixtures that only serialize hand-built
payload structs and compare them with checked-in JSON produced from the same code.

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
