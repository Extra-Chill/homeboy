---
name: homeboy
description: "Use Homeboy CLI for version management, deployment, fleet operations, documentation tooling, database ops, extension management, code auditing, and remote file/log access."
compatibility: "Cross-platform Rust CLI. Works with any language/framework. Requires SSH for remote operations."
---

# Homeboy CLI

Development and deployment automation. Generic — works with any language/framework.

## How to Use This Skill

**Do not memorize commands.** Homeboy has built-in help at every level. Discover what you need:

```bash
homeboy --help                      # list all top-level commands
homeboy <command> --help            # subcommands and options for any command
homeboy <command> <subcommand> --help  # detailed usage for any subcommand
```

Every command returns structured JSON: `{"success": true, "data": {...}}`.
Errors include codes and hints: `{"success": false, "error": {"code": "...", "message": "...", "hints": [...]}}`.

## Entity Hierarchy

```
Component  →  versioned, deployable unit (plugin, CLI, extension)
Project    →  deployment target (site on a server, links to components)
Server     →  SSH connection config
Fleet      →  named group of projects (batch operations)
Extension  →  installable plugin (CLI scripts, actions, docs)
```

Storage: `~/.config/homeboy/{components,projects,servers,fleets}/<id>.json`

## Core Workflows

### Version + Release
Add changelog entries, bump versions, tag, and push — all in one flow. Homeboy manages version targets (regex patterns that work on any file format), changelog finalization, git commit, tag, and push.

```bash
homeboy changelog --help   # how to add entries
homeboy version --help     # how to bump
homeboy changes --help     # what changed since last tag
homeboy release --help     # interactive release planning
```

### Deploy
Deploy components to individual projects, entire fleets, or all projects that use a component. Supports build commands, artifact upload, and dry-run checks.

```bash
homeboy deploy --help
```

### Fleet Management
Group projects into fleets for batch operations, drift detection, and status checks.

```bash
homeboy fleet --help
```

### Status + Audit + Cleanup
- `homeboy status` — actionable overview of components (uncommitted, needs-bump, ready to deploy)
- `homeboy audit` — discover conventions per directory, detect drift and outliers
- `homeboy cleanup` — identify config drift and stale state

```bash
homeboy status --help
homeboy audit --help
homeboy cleanup --help
```

### Documentation Tooling
Scaffold coverage analysis, audit doc claims against code, bulk-generate docs.

```bash
homeboy docs --help
```

### Extensions
Installable plugins that add CLI tools, actions, and project-type support.

```bash
homeboy extension --help
```

### Remote Operations
File ops, log viewing, database queries, SSH, and file transfer — all over SSH tunnels.

```bash
homeboy file --help
homeboy logs --help
homeboy db --help
homeboy ssh --help
homeboy transfer --help
```

### Remote Tool Bridges
Run project-specific tools (WP-CLI, PM2, OpenClaw, Cargo, Sweatpants) on remote servers without logging in. These appear as top-level commands based on installed extensions.

```bash
homeboy --help              # bridge commands appear at the bottom
homeboy wp --help           # WordPress CLI bridge
homeboy pm2 --help          # PM2 bridge
homeboy openclaw --help     # OpenClaw bridge
```

### API Client
Authenticate with project APIs and make REST requests.

```bash
homeboy auth --help
homeboy api --help
```

### Git, Build, Test
```bash
homeboy git --help
homeboy build --help
homeboy test --help
homeboy lint --help
```

### Config + Upgrade
```bash
homeboy config --help
homeboy upgrade --help
```

## Key Principles

- **Version targets are regex patterns** — they work on any file (Cargo.toml, package.json, PHP headers, etc.)
- **Components and extensions support lifecycle hooks** — `hooks` map with events like `pre:version:bump`, `post:deploy`, etc.
- **Audit discovers conventions automatically** — naming, imports, methods, registrations per directory
- **All output is JSON** — pipe to `jq` or parse programmatically
- **When in doubt, run `--help`** — it's always accurate and up to date
