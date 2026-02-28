# Homeboy Extensions

Official extension directory for [Homeboy](https://github.com/Extra-Chill/homeboy). Extensions extend the CLI with project-type support — WordPress, Node.js, Rust, and more.

This is a **monorepo** — each subdirectory is a standalone extension. Install individual extensions, not the whole repo.

## Available Extensions

| Extension | Install command | Description |
|--------|----------------|-------------|
| `wordpress` | `homeboy extension install … --id wordpress` | WP-CLI integration, build, test, lint |
| `nodejs` | `homeboy extension install … --id nodejs` | PM2 process management |
| `rust` | `homeboy extension install … --id rust` | Cargo CLI integration |
| `github` | `homeboy extension install … --id github` | GitHub CLI for issues, PRs, and repos |
| `homebrew` | `homeboy extension install … --id homebrew` | Homebrew tap publishing |
| `agent-hooks` | `homeboy extension install … --id agent-hooks` | AI agent guardrails (Claude Code, OpenCode) |
| `openclaw` | `homeboy extension install … --id openclaw` | OpenClaw AI agent integration |
| `sweatpants` | `homeboy extension install … --id sweatpants` | Sweatpants automation engine bridge |

## Installation

Install extensions using the Homeboy CLI:

```bash
# Install a single extension from this monorepo
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id wordpress

# Install multiple extensions
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id github
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust
```

Homeboy clones the repo, detects the monorepo layout, and extracts just the extension you asked for into `~/.config/homeboy/extensions/<id>/`.

### Verify installation

```bash
# List all installed extensions
homeboy extension list

# Inspect a specific extension
homeboy extension show wordpress
```

### Install from a local clone

If you prefer to clone the repo first:

```bash
git clone https://github.com/Extra-Chill/homeboy-extensions.git
homeboy extension install ./homeboy-extensions/wordpress
homeboy extension install ./homeboy-extensions/github
```

## Usage

Once installed, use the extension's tool against any project or component:

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

## Creating Extensions

Each extension is a directory containing a `<extension-id>.json` manifest. The manifest defines capabilities, commands, and settings. See existing extensions for examples.

Extension docs are optional — not every extension includes embedded markdown docs.
