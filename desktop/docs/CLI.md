# Homeboy Desktop ↔ CLI integration

Homeboy Desktop shells out to the standalone `homeboy` CLI binary for most core operations.

## CLI installation / discovery

The desktop app also uses a 30s default timeout for CLI commands (`CLIBridge.execute`, `CLIBridge.executeWithStdin`).

The desktop app relies on a system-installed `homeboy` binary and checks these paths in order:

- `/opt/homebrew/bin/homeboy` (Apple Silicon Homebrew)
- `/usr/local/bin/homeboy` (Intel Homebrew)
- `~/.cargo/bin/homeboy` (Cargo)

Verify installation:

```bash
homeboy --version
```

CLI source: `../homeboy/` (Rust workspace; the CLI implements the canonical command/output contracts used by the desktop app).

## Shared configuration

Homeboy Desktop is macOS-only, but it interoperates with the CLI by reading/writing configuration in the same general filesystem area; the CLI’s canonical config root is `dirs::config_dir()/homeboy`.

Canonical config path rules live in the CLI docs: [`homeboy/docs/index.md`](../../homeboy/docs/index.md).

macOS filesystem locations:

- **Desktop config root** (current implementation): `~/Library/Application Support/Homeboy/` (see `Homeboy/Core/Config/AppPaths.swift`)

The desktop config tree:

```
~/Library/Application Support/Homeboy/
├── projects/             # Project configurations
├── servers/              # SSH server configurations
├── components/           # Component definitions
├── modules/              # Installed modules
├── keys/                 # SSH keys (per server)
├── project-types/        # Project type definitions
├── playwright-browsers/  # Playwright downloads (module runtime)
├── venv/                 # Shared python venv (if used)
└── backups/              # Backup files
```

Projects and servers are editable via Homeboy.app.

The CLI has its own cross-platform config root (documented in `homeboy/docs/index.md`); don’t assume it shares the exact same on-disk layout as the desktop app.
## CLI reference

This document avoids duplicating CLI command docs.

Use the CLI as the source of truth:
- `homeboy docs`
- `homeboy docs <topic>`
- Markdown sources embedded into the CLI: [`homeboy/docs/`](../../homeboy/docs/index.md)

### Desktop ↔ CLI responsibilities

- The desktop app reads/writes configuration JSON under `~/Library/Application Support/Homeboy/` (see `AppPaths` and `ConfigurationObserver`).
- The desktop app executes the CLI via `CLIBridge` and expects JSON output for most operations.
- The UI reacts to on-disk changes via `ConfigurationObserver` and publishes `ConfigurationChangeType` events.

If you edit config via CLI/scripts, verify the desktop app is pointed at the same paths; `AppPaths` is the desktop single source of truth.
### Config editing notes

The desktop app’s Settings UI is the intended way to create and edit projects/servers.

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
