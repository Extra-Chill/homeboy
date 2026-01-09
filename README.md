# Homeboy CLI

CLI tool for development and deployment automation.

## Installation

### Homebrew (macOS/Linux)
```bash
brew tap extra-chill/tap
brew install homeboy
```

### Cargo (requires Rust)
```bash
cargo install homeboy
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
| `ssh` | SSH into project server |
| `wp` | Run WP-CLI commands on WordPress projects |
| `pm2` | Run PM2 commands on Node.js projects |
| `db` | Database operations |
| `file` | Remote file operations |
| `logs` | Remote log viewing |
| `deploy` | Deploy components to remote server |
| `pin` | Manage pinned files and logs |
| `module` | Execute CLI-compatible modules |
| `docs` | Display CLI documentation |

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

Configuration is stored in:
- **macOS**: `~/Library/Application Support/Homeboy/`
- **Linux**: `~/.local/share/Homeboy/`

```
Homeboy/
├── config.json           # Active project ID
├── projects/             # Project configurations
├── servers/              # Server configurations
├── components/           # Component configurations
├── modules/              # Installed modules
└── keys/                 # SSH keys
```

## License

MIT
