# Homeboy CLI

Homeboy is a config-driven automation engine for repetitive discovery and AI coding workflows.

It standardizes common operations (project/component discovery, remote execution, deployments) and typically produces machine-readable output via a stable JSON envelope.

**Note:** This is experimental and still early in development. Breaking changes may be frequent before 0.5.0.

## Installation

### Homebrew
```bash
brew tap extra-chill/tap
brew install homeboy
```

This installs the **Homeboy CLI** (`homeboy`). It does not install the macOS desktop app.

### Cargo (requires Rust)
```bash
cargo install --path crates/homeboy
```

### Direct Download
Download from [GitHub Releases](https://github.com/Extra-Chill/homeboy-cli/releases).

## Commands

See:

- [Docs index](docs/index.md)
- [Commands index](docs/commands/commands-index.md)
- [JSON output contract](docs/json-output/json-output-contract.md)

## Agent instructions

Homeboy includes agent-oriented instruction files under `agent-instructions/`.

- `agent-instructions/skills/`: skills that can be added to LLM coding agents
- `agent-instructions/commands/`: reusable command recipes (slash-command style)

These files are not used by Homeboy at runtime; they are meant for humans and coding agents.

## Usage

See [CLI documentation](docs/index.md) for the authoritative command list and JSON contracts.

A few common entrypoints:

```bash
homeboy project list
homeboy project create "My Project" example.com --module wordpress --activate
homeboy project set <projectId> --domain example.com --server-id <serverId>
homeboy project repair <projectId>
homeboy project switch <projectId>
homeboy wp <projectId> core version
homeboy deploy <projectId> --dry-run --all
homeboy ssh <projectId>
homeboy module list
homeboy docs --list
homeboy docs commands/deploy
```

## Configuration

Configuration and state live under the OS config directory (from `dirs::config_dir()`), under a `homeboy/` folder:

- **macOS**: `~/Library/Application Support/homeboy/`
- **Linux**: `$XDG_CONFIG_HOME/homeboy/` (fallback: `~/.config/homeboy/`)
- **Windows**: `%APPDATA%\\homeboy\\`

Common paths:

- `homeboy.json`
- `projects/`
- `servers/`
- `components/`
- `modules/`
- `keys/`
- `backups/`

See [CLI documentation](docs/index.md) for details.

## SSH

Homeboy connects over SSH using server configuration stored under `homeboy/servers/`.

Key management commands (generate/import/use/unset/show) are documented in [server](docs/commands/server.md).

## License

MIT
