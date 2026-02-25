# Homeboy Modules

Official module directory for [Homeboy](https://github.com/Extra-Chill/homeboy). Modules extend the CLI with project-type support — WordPress, Node.js, Rust, and more.

This is a **monorepo** — each subdirectory is a standalone module. Install individual modules, not the whole repo.

## Available Modules

| Module | Install command | Description |
|--------|----------------|-------------|
| `wordpress` | `homeboy module install … --id wordpress` | WP-CLI integration, build, test, lint |
| `nodejs` | `homeboy module install … --id nodejs` | PM2 process management |
| `rust` | `homeboy module install … --id rust` | Cargo CLI integration |
| `github` | `homeboy module install … --id github` | GitHub CLI for issues, PRs, and repos |
| `homebrew` | `homeboy module install … --id homebrew` | Homebrew tap publishing |
| `agent-hooks` | `homeboy module install … --id agent-hooks` | AI agent guardrails (Claude Code, OpenCode) |
| `openclaw` | `homeboy module install … --id openclaw` | OpenClaw AI agent integration |
| `sweatpants` | `homeboy module install … --id sweatpants` | Sweatpants automation engine bridge |

## Installation

Install modules using the Homeboy CLI:

```bash
# Install a single module from this monorepo
homeboy module install https://github.com/Extra-Chill/homeboy-modules --id wordpress

# Install multiple modules
homeboy module install https://github.com/Extra-Chill/homeboy-modules --id github
homeboy module install https://github.com/Extra-Chill/homeboy-modules --id rust
```

Homeboy clones the repo, detects the monorepo layout, and extracts just the module you asked for into `~/.config/homeboy/modules/<id>/`.

### Verify installation

```bash
# List all installed modules
homeboy module list

# Inspect a specific module
homeboy module show wordpress
```

### Install from a local clone

If you prefer to clone the repo first:

```bash
git clone https://github.com/Extra-Chill/homeboy-modules.git
homeboy module install ./homeboy-modules/wordpress
homeboy module install ./homeboy-modules/github
```

## Usage

Once installed, use the module's tool against any project or component:

```bash
# WordPress
homeboy wp my-site plugin list

# Node.js
homeboy pm2 my-app restart

# Rust
homeboy cargo my-crate build

# GitHub
homeboy gh my-repo pr list
```

## Creating Modules

Each module is a directory containing a `<module-id>.json` manifest. The manifest defines capabilities, commands, and settings. See existing modules for examples.

Module docs are optional — not every module includes embedded markdown docs.
