# Homeboy CLI

Homeboy is a config-driven automation CLI designed to be used during agentic coding sessions.

It standardizes the “plumbing” work that slows iteration down—project/component discovery, reusable module tools, SSH passthrough, deployments, database operations—while keeping output machine-oriented (most commands return a stable JSON envelope).

Homeboy is intended to make it easier to iterate quickly across many projects at once, with deep automation capabilities provided by modules and CLI tools.

**Note:** This is still early in development. Breaking changes may occur between releases.

## Agent usage (recommended)

Add this to your agent instructions (AGENTS.md, CLAUDE.md, .cursorrules, etc.):

> Use Homeboy for deployments, server/db ops, version/build management, and documentation.
> Run `homeboy init` first to understand context, then `homeboy docs <topic>` for commands.
> For documentation tasks, see `homeboy docs documentation/index`.

This keeps agent actions consistent and prevents assumptions about project structure.

## `homeboy init`

`homeboy init` is a setup helper intended to reduce first-run friction. Depending on your environment it can guide you through (or scaffold) common configuration so you can start using commands like `project`, `server`, `ssh`, and modules sooner.

See: `homeboy docs commands/init`.

## Installation

See the monorepo-level documentation for installation options:

- [Root README](../README.md)
- [Homebrew tap](../homebrew-tap/README.md)

## Usage

See [CLI documentation](docs/index.md) for the authoritative command list, embedded docs topics (`homeboy docs ...`), and JSON contracts.

A few common entrypoints:

```bash
homeboy docs
homeboy docs --list
homeboy docs commands/project

homeboy list
homeboy project list
homeboy component list
homeboy module list

homeboy pm2 <project_id> status
homeboy wp <project_id> plugin list

homeboy changes <component_id>

homeboy ssh <project_id>                 # interactive passthrough
homeboy logs show <project_id> <path> --follow   # interactive passthrough
```

## Docs

- [Docs index](docs/index.md)
- [Commands index](docs/commands/commands-index.md)
- [JSON output contract](docs/json-output/json-output-contract.md)

## Configuration

Configuration and state live under the OS config directory (from `dirs::config_dir()`), under a `homeboy/` folder.

Common defaults:

- **macOS**: `~/Library/Application Support/homeboy/`
- **Linux**: `$XDG_CONFIG_HOME/homeboy/` (fallback: `~/.config/homeboy/`)
- **Windows**: `%APPDATA%\\homeboy\\`

Common paths:

- `projects/<id>.json`, `servers/<id>.json`, and `components/<id>.json` under the Homeboy config root
- `modules/<module_id>/<module_id>.json` (module manifest; used by the module system)

Notes:

- Homeboy does not use a global project config file like `./homeboy.json`.
- Persistent configuration is stored as JSON records under `projects/`, `servers/`, and `components/`.
- Homeboy stores persistent SSH keys under `keys/`.

See [CLI documentation](docs/index.md) for details.

## SSH

Homeboy connects over SSH using server configuration stored under `servers/` inside the OS config directory.

Key management commands (generate/import/use/unset/show) are documented in [server](docs/commands/server.md).

## License

See `../LICENSE` for the repository license.

Note: `homeboy` (CLI) is distributed under the same license as the rest of this repository unless explicitly stated otherwise in build/distribution tooling.
