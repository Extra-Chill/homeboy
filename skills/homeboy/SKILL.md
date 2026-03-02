---
name: homeboy
description: "Config-driven CLI for multi-project deployment, versioning, and development automation. Structured JSON output, embedded docs, predictable contracts."
compatibility: "Cross-platform Rust CLI. Works with any language/framework. Requires SSH for remote operations."
---

# Homeboy CLI

## Prerequisites

Before using any Homeboy commands, verify it is installed and accessible:

```bash
homeboy --version
```

If this fails, Homeboy is not installed. Install via Homebrew or from source:

```bash
# Homebrew
brew tap Extra-Chill/homebrew-tap && brew install homeboy

# From source (requires Rust toolchain)
git clone https://github.com/Extra-Chill/homeboy.git && cd homeboy && cargo install --path .
```

Do not proceed until `homeboy --version` succeeds.

## How to Discover Everything

Homeboy embeds its complete documentation in the binary. You never need external docs, READMEs, or source code. Everything is discoverable at runtime.

### Command help

```bash
homeboy --help                           # all top-level commands
homeboy <command> --help                 # subcommands and flags for any command
homeboy <command> <subcommand> --help    # detailed usage for any subcommand
```

### Embedded docs

```bash
homeboy docs list                        # all available doc topics
homeboy docs <topic>                     # read any topic
```

This is the authoritative reference. Topics cover commands, JSON schemas, architecture, and developer guides. Start here:

```bash
homeboy docs commands/commands-index     # full command reference
homeboy docs schemas/component-schema    # component config schema
homeboy docs schemas/project-schema      # project config schema
homeboy docs schemas/server-schema       # server config schema
homeboy docs schemas/fleet-schema        # fleet config schema
homeboy docs schemas/extension-manifest-schema  # extension schema
homeboy docs architecture/output-system  # JSON output contract
homeboy docs architecture/hooks          # lifecycle hook system
homeboy docs architecture/release-pipeline      # release pipeline internals
homeboy docs architecture/keychain-secrets      # secrets/keychain system
```

Read these docs before guessing at flags or output shapes. They are compiled into the binary and always match the installed version.

## Output Contract

Every command returns a stable JSON envelope:

```json
{"success": true, "data": { ... }}
```

```json
{"success": false, "error": {"code": "...", "message": "...", "hints": [...]}}
```

Error codes are stable and namespaced (`config.*`, `ssh.*`, `deploy.*`, `git.*`, `internal.*`). The `data` shape is command-specific — read `homeboy docs commands/<command>` for the exact structure.

Exceptions: `homeboy docs` outputs raw markdown. `homeboy ssh` and `homeboy logs --follow` use interactive passthrough.

## Data Model

```
Component  →  versioned, deployable unit (plugin, theme, CLI, package)
Project    →  deployment target (site on a server, links to components)
Server     →  SSH connection config
Fleet      →  named group of projects for batch operations
Extension  →  installable plugin (adds CLI commands, platform behaviors, hooks, docs)
```

All config is JSON files in `~/.config/homeboy/`. Run `homeboy docs schemas/<entity>-schema` for the exact shape of each.

## Rules

- **Do not memorize commands.** Run `--help` or `homeboy docs` to discover what you need.
- **Do not edit version files manually.** Use `homeboy version bump` — it manages version targets across multiple files.
- **Do not deploy manually.** Use `homeboy deploy` — it builds, uploads, and runs post-deploy hooks.
- **Start with `homeboy init` or `homeboy status`** to understand the current state before operating.
- **Use `--dry-run`** on destructive operations (`deploy`, `release`, `refactor rename`) to preview before executing.
- **All output is JSON** — parse it programmatically, don't scrape text.
