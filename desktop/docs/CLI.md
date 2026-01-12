# Homeboy Desktop + CLI Integration

Homeboy Desktop shells out to the standalone `homeboy` CLI binary for all core operations.

## Installation Notes

The CLI is maintained as a standalone project (`homeboy-cli/`). The desktop app shells out to a system-installed `homeboy` binary.

### CLI Binary Location

The CLI is maintained as a standalone project (`homeboy-cli/`). The desktop app shells out to the `homeboy` CLI binary installed on your system.

**Supported Paths**:
- `/opt/homebrew/bin/homeboy` (Apple Silicon Homebrew)
- `/usr/local/bin/homeboy` (Intel Homebrew)
- `~/.cargo/bin/homeboy` (Cargo)

The app checks these paths in order. If `homeboy` is not found, the desktop app prompts you to install it.

Verify installation:
```bash
homeboy --version
```

### CLI Source

The CLI source lives in `homeboy-cli/`.

## Configuration

The CLI uses the same configuration as the desktop app stored at:
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

Projects and servers can be configured via Homeboy.app or via the CLI.

## Commands

This document intentionally avoids duplicating CLI reference docs.

For the canonical CLI reference, use:

```bash
homeboy docs
```

Or browse the markdown source under `homeboy-cli/docs/commands/`.

### Desktop ↔ CLI responsibilities

- The desktop app reads/writes configuration JSON under `~/Library/Application Support/homeboy/`.
- The desktop app executes the CLI via `CLIBridge` and expects JSON output for most operations.
- The CLI can also manage config directly; the app should react via its directory watchers.

### Common CLI entrypoints

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

(See `homeboy docs <topic>` for each command’s details.)

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

Deploy configured components to the project server.

```bash
homeboy deploy <project> [component-id...] [flags]
```

**Arguments**:
- `project` - Project ID (required)
- `component-id` - One or more component IDs to deploy (optional if using flags)

**Flags**:
- `--all` - Deploy all configured components
- `--outdated` - Deploy only components where local version differs from remote
- `--dry-run` - Show what would be deployed without executing

Note: `homeboy deploy` always returns JSON output wrapped in the global envelope; there is no `--json` flag and no `--build` flag. Use `homeboy build <componentId>` separately if you want to build before deploying.

**Deployment Process**:
- Upload the configured artifact to the remote server.
- If the component config includes an extract command, the uploaded artifact may be extracted on the remote server.

**Requirements**:
- Components configured with their local path, remote path, and build artifact path in Homeboy config
- Server configured with SSH key

**Examples**:
```bash
homeboy deploy extrachill my-plugin my-theme
homeboy deploy extrachill --all
homeboy deploy extrachill --outdated
homeboy deploy extrachill --all --dry-run
```

For the canonical deploy JSON output contract, run `homeboy docs deploy` (or see `homeboy-cli/docs/commands/deploy.md`).

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

For canonical pin management (files/logs), run `homeboy docs pin`.

### docs

For the full CLI reference, run `homeboy docs`.

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error (configuration missing, command failed, etc.) |

## Error Messages

| Error | Notes |
|-------|-------|
| Project not found | Create the project (CLI) or configure it in the desktop app |
| Server not configured | Link a server on the project configuration |
| SSH key not found | Generate an SSH key for the server in the desktop app |
| Cannot delete active project | Switch active project, then delete |
| Module not found | List installed modules and verify the module ID |
