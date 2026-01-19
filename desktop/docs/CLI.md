# Homeboy Desktop ↔ CLI integration

Homeboy Desktop shells out to the standalone `homeboy` CLI binary for most core operations.

## CLI installation / discovery

The desktop app also uses a 30s default timeout for CLI commands (`CLIBridge.execute`, `CLIBridge.executeWithStdin`).

The desktop app relies on a system-installed `homeboy` binary and checks these paths in order:

- `/opt/homebrew/bin/homeboy` (Apple Silicon Homebrew)
- `/usr/local/bin/homeboy` (Intel Homebrew / manual)
- `~/.cargo/bin/homeboy` (Cargo)

Verify installation:

```bash
homeboy --version
```

CLI source: `../homeboy/` (Rust workspace; the CLI implements the canonical command/output contracts used by the desktop app).

## Shared configuration

Homeboy Desktop uses CLI for all configuration operations. The CLI manages the canonical configuration at `~/.config/homeboy/`. Desktop app reads/writes configuration via CLI commands through CLIBridge.

Configuration directory: `~/.config/homeboy/` (managed by CLI, universal across platforms)

macOS filesystem location:

- **Shared config**: `~/.config/homeboy/`

The desktop app does NOT have its own config tree. All storage is handled by the CLI.

## CLI reference

This document avoids duplicating CLI command docs.

Use the CLI as the source of truth:
- `homeboy docs`
- `homeboy docs <topic>`
- Markdown sources embedded into the CLI: [`homeboy/docs/`](../../homeboy/docs/index.md)

### Desktop ↔ CLI responsibilities

- The desktop app executes the CLI via `CLIBridge` and expects JSON output for most operations.
- The UI reacts to on-disk changes via `ConfigurationObserver` and publishes `ConfigurationChangeType` events.
- The desktop app does NOT write configuration files directly. All storage goes through CLI commands.

### Config editing notes

The desktop app's Settings UI is the intended way to create and edit projects/servers.

If you do edit config via CLI/scripts, use `homeboy docs projects`, `homeboy docs project`, and `homeboy docs server` for the canonical flag list and schema expectations.

### server

The desktop app expects servers to be editable in Settings, and uses the CLI for operations that require SSH (connection tests, remote file/log access, deployments).

For canonical server command docs and JSON output, run `homeboy docs server`.

### wp / pm2 / db / deploy / ssh / logs / file / module

These areas are implemented by the CLI and surfaced by the desktop app via `CLIBridge`.

For canonical flags, output schemas, safety rules, and subtarget behavior, use `homeboy docs` / `homeboy docs <topic>`.

## Exit codes

The desktop app relies on the CLI's mapped exit codes.

Canonical mapping and error-code groups live in:
- [`homeboy/docs/json-output/json-output-contract.md`](../../homeboy/docs/json-output/json-output-contract.md#exit-codes)

## Error reporting

The desktop app surfaces CLI failures as `AppError` instances (see [ERROR-HANDLING](ERROR-HANDLING.md)).

Canonical error codes/messages and exit code mapping live in the CLI docs:
- [`homeboy/docs/json-output/json-output-contract.md#exit-codes`](../../homeboy/docs/json-output/json-output-contract.md#exit-codes)
