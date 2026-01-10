# Homeboy CLI

CLI tool for development and deployment automation. Component-scoped operations for the entire local dev pipeline.

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

| Command | Description |
|---------|-------------|
| `projects` | List all configured projects |
| `project` | Manage project configuration |
| `server` | Manage SSH server configurations |
| `component` | Manage standalone component configurations |
| `git` | Component-scoped git operations |
| `version` | Component-scoped version management |
| `build` | Component-scoped builds |
| `deploy` | Deploy components to remote server |
| `ssh` | SSH into project server |
| `wp` | Run WP-CLI commands on WordPress projects |
| `pm2` | Run PM2 commands on Node.js projects |
| `db` | Database operations |
| `file` | Remote file operations |
| `logs` | Remote log viewing |
| `pin` | Manage pinned files and logs |
| `module` | Execute CLI-compatible modules |
| `docs` | Display CLI documentation |

## Local Dev Pipeline

Homeboy provides a unified interface for component-scoped local development - no more `cd` ceremony:

```bash
# Version management
homeboy version show my-plugin           # Display current version
homeboy version bump my-plugin patch     # 0.1.2 → 0.1.3

# Git operations
homeboy git status my-plugin             # Show git status
homeboy git commit my-plugin "message"   # Stage all and commit
homeboy git push my-plugin               # Push to remote
homeboy git tag my-plugin v1.0.0         # Create tag
homeboy git push my-plugin --tags        # Push with tags

# Build
homeboy build my-plugin                  # Run component's build_command

# Deploy
homeboy deploy myproject my-plugin       # Deploy to remote server
```

## Usage

```bash
# List projects
homeboy projects

# Switch active project
homeboy project switch myproject

# Run WP-CLI command
homeboy wp myproject core version

# Deploy a component
homeboy deploy myproject my-plugin

# SSH into server
homeboy ssh myproject

# View logs
homeboy logs show myproject debug.log -f
```

## Configuration

Configuration is stored in the Homeboy data directory (via `dirs::data_dir()`):
- **macOS**: `~/Library/Application Support/Homeboy/`
- **Linux**: `~/.local/share/Homeboy/` (exact path varies by distribution)

The CLI binary is installed to `/usr/local/bin/homeboy` (system) or `/opt/homebrew/bin/homeboy` (Homebrew ARM).

The macOS desktop app (if installed) uses the same directory and JSON structure, but it is not required for CLI usage.

Run `homeboy docs` or view the [CLI documentation](crates/homeboy/docs/index.md) for detailed information.

```
Homeboy/
├── config.json           # Active project ID
├── projects/             # Project configurations
├── servers/              # Server configurations
├── components/           # Component configurations
├── modules/              # Installed modules
└── keys/                 # SSH keys (e.g. server-foo-bar_id_rsa)
```

## SSH Setup

By default, Homeboy uses your system SSH configuration (including `~/.ssh/config`, SSH agent, Keychain, 1Password, etc.). No Homeboy-managed key file is required.

Optional: configure an explicit identity file for a server:

```bash
# Use an existing private key path (does not copy the key)
homeboy server key use server-example-com ~/.ssh/id_ed25519

# Revert to normal SSH resolution
homeboy server key unset server-example-com
```

Optional: have Homeboy generate or import a key into the Homeboy data directory and set it for the server:

```bash
# Generate a new keypair
homeboy server key generate server-example-com

# Or import an existing private key (Homeboy copies it into Homeboy/keys/)
homeboy server key import server-example-com ~/.ssh/id_rsa
```

To print the public key (for `~/.ssh/authorized_keys`):

```bash
homeboy server key show server-example-com --raw
```

## License

MIT
