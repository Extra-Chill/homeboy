# Homeboy Modules

Installable modules for [Homeboy](https://github.com/Extra-Chill/homeboy) that extend CLI tool support.

## Available Modules

| Module | Tool | Description |
|--------|------|-------------|
| `wordpress` | `wp` | WP-CLI integration with database discovery |
| `nodejs` | `pm2` | PM2 process management |
| `rust` | `cargo` | Cargo CLI integration |
| `github` | `gh` | GitHub CLI for issues, PRs, and repos |
| `homebrew` | `brew` | Homebrew tap publishing |

## Installation

Clone or symlink modules to your Homeboy config directory:

```bash
# Clone the repo
git clone https://github.com/Extra-Chill/homeboy-modules.git

# Symlink a module
ln -s /path/to/homeboy-modules/github ~/.config/homeboy/modules/github
```

## Usage

Once installed, use the module's tool against any component:

```bash
# WordPress
homeboy wp my-site plugin list

# Node.js
homeboy pm2 my-app restart

# Rust
homeboy cargo my-crate build

# GitHub
homeboy gh my-component issue list
homeboy gh my-component pr status
```

## Creating Modules

Each module is a directory containing a `homeboy.json` manifest. See existing modules for examples.

Note: not every module includes embedded markdown docs; module docs are optional.
