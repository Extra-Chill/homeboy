# Homeboy Desktop ↔ CLI integration

Homeboy Desktop shells out to the standalone `homeboy` CLI binary for most core operations.

## CLI installation / discovery

The desktop app relies on a system-installed `homeboy` binary and checks these paths in order:

- `/opt/homebrew/bin/homeboy` (Apple Silicon Homebrew)
- `/usr/local/bin/homeboy` (Intel Homebrew)
- `~/.cargo/bin/homeboy` (Cargo)

Verify installation:

```bash
homeboy --version
```

CLI source: `../homeboy-cli/`.

## Shared configuration

Homeboy Desktop is macOS-only, but it interoperates with the CLI in the same on-disk config tree used by the CLI.

Canonical config path rules live in the CLI docs: [`homeboy-cli/docs/index.md`](../../homeboy-cli/docs/index.md).

macOS config location:

```
~/Library/Application Support/homeboy/
├── homeboy.json          # App config
├── projects/             # Project configurations
├── servers/              # SSH server configurations
├── components/           # Component definitions
├── modules/              # Installed modules
├── keys/                 # SSH keys (per server)
└── backups/              # Backup files
```

Projects and servers are editable via Homeboy.app or via the CLI.

## CLI reference

This document avoids duplicating CLI command docs.

- Canonical CLI reference: `homeboy docs`
- Markdown sources embedded into the CLI: [`homeboy-cli/docs/`](../../homeboy-cli/docs/index.md)

### Desktop ↔ CLI responsibilities

- The desktop app reads/writes configuration JSON under `~/Library/Application Support/homeboy/`.
- The desktop app executes the CLI via `CLIBridge` and expects JSON output for most operations.
- The CLI can also manage config directly; the app should react via its directory watchers.

### Common CLI entrypoints

(See `homeboy docs <topic>` for canonical flags, schemas, and JSON output.)

```bash
homeboy docs
homeboy project list
homeboy project show <projectId>
homeboy server list
homeboy deploy <projectId>
homeboy ssh <projectId>
homeboy db <projectId> tables
homeboy logs list <projectId>
homeboy file list <projectId>
```
### Config editing notes

The desktop app’s Settings UI is the intended way to create and edit projects/servers.

If you do edit config via CLI/scripts, use `homeboy docs projects`, `homeboy docs project`, and `homeboy docs server` for the canonical flag list and schema expectations.

### server

The desktop app expects servers to be editable in Settings, and uses the CLI for operations that require SSH (connection tests, remote file/log access, deployments).

For canonical server command docs and JSON output, run `homeboy docs server`.

### wp

The desktop app shells out to the CLI for WordPress operations (WP-CLI) when needed.

For canonical usage, subtarget rules, and JSON output, run `homeboy docs wp`.

### pm2

For canonical PM2 usage, run `homeboy docs pm2`.

### db

The desktop app’s Database Browser uses the CLI for table listing, schema descriptions, and query execution.

For canonical usage, safety rules (`query` vs destructive subcommands), and JSON output, run `homeboy docs db`.

### deploy

The desktop app uses `homeboy deploy` for component deployments.

This document does not restate deploy flags or JSON shapes; the canonical reference is:

- `homeboy docs commands/deploy`
- [`homeboy-cli/docs/commands/deploy.md`](../../homeboy-cli/docs/commands/deploy.md)

Note: deploy output is JSON-wrapped like other commands; there is no extra `--json` flag. Building artifacts is a separate concern (`homeboy build`).

### ssh

The desktop app uses the CLI for SSH connectivity checks and project-scoped remote operations.

For canonical behavior (project vs server resolution) and JSON output, run `homeboy docs ssh`.

### git

For canonical git helper usage, run `homeboy docs git`.

### version

For canonical version management usage, run `homeboy docs version`.

### module

For canonical module CLI usage, run `homeboy docs module`.

## Subtarget Support

Some CLI commands accept an optional subtarget identifier (used for things like WordPress multisite and Node environments).

For canonical subtarget resolution rules and examples, run `homeboy docs project` and `homeboy docs wp`/`homeboy docs pm2`.

### component

For canonical component configuration commands, run `homeboy docs component`.

### logs

For canonical remote log tooling, run `homeboy docs logs`.

### file

For canonical remote file tooling, run `homeboy docs file`.

### pin

Pin management lives under `homeboy project pin ...`.

For canonical pin docs, run `homeboy docs commands/project` and see the `project pin` section.
### docs

For the full CLI reference, run `homeboy docs`.

## Exit codes

The desktop app relies on the CLI's mapped exit codes.

Canonical mapping and error-code groups live in:
- [`homeboy-cli/docs/json-output/json-output-contract.md`](../../homeboy-cli/docs/json-output/json-output-contract.md#exit-codes)

## Error Messages

| Error | Notes |
|-------|-------|
| Project not found | Create the project (CLI) or configure it in the desktop app |
| Server not configured | Link a server on the project configuration |
| SSH key not found | Generate an SSH key for the server in the desktop app |
| Cannot delete active project | Switch active project, then delete |
| Module not found | List installed modules and verify the module ID |
