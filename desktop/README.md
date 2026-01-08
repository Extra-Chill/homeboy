# Homeboy

Native macOS SwiftUI application for WordPress development and deployment automation. Configure multiple WordPress sites and extend functionality with installable modules.

## Features

### Core Tools

**Deployer**
One-click deployment of WordPress plugins and themes to any SSH-accessible server.
- SSH key authentication
- Build automation via `build.sh` scripts
- Version comparison (local vs remote)
- Per-site component registry

**WP-CLI Terminal**
Execute WP-CLI commands on Local by Flywheel sites.
- Real-time output streaming
- Command history navigation
- Environment detection for Local by Flywheel PHP/MySQL paths

**Database Browser**
Browse and query remote MySQL databases over SSH tunnel.
- Table listing with WordPress multisite support
- Query editor with results display
- Row selection and clipboard operations

**Config Editor**
Edit project configuration JSON files with syntax highlighting and backup support.

**Debug Logs**
View and search WordPress debug logs from remote servers.

### Module System

Extend functionality with installable modules. Modules are self-contained plugins with:
- JSON manifest defining inputs, outputs, and actions
- Isolated Python virtual environments
- Dynamic UI generation from manifest
- API action support for WordPress REST endpoints

See [docs/MODULE-SPEC.md](docs/MODULE-SPEC.md) for the complete module specification.

### Command Line Tool

Homeboy includes a CLI (`homeboy`) for terminal access to project management, WordPress operations, database queries, and deployments.

**Installation**: The app prompts to install the CLI on first launch, or install manually via **Settings > General**.

**Available Commands**:
```bash
homeboy projects              # List configured projects
homeboy wp <project> <cmd>    # Execute WP-CLI on production
homeboy db <project> tables   # List database tables
homeboy deploy <project> --all  # Deploy all components
homeboy ssh <project>         # Open interactive SSH shell
```

Projects must be configured via Homeboy.app before using the CLI. See [docs/CLI.md](docs/CLI.md) for full documentation.

## Requirements

- macOS 14.4+ (Sonoma)
- Xcode 15.0+
- Python 3.12+ (Homebrew) - for modules with Python scripts
- Homebrew packages: `wp-cli` (used by WP-CLI features)

## Setup

### 1. Clone and Open

```bash
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy
open Homeboy.xcodeproj
```

### 2. Build and Run

Build and run from Xcode (Cmd+R).

### 3. Configure Your WordPress Sites

Homeboy works with **projects** (site profiles). On first launch, configure your project in **Settings**:
- **General**: Project name and local domain
- **Servers**: Add an SSH server (host/user/port) and generate an SSH key
- **WordPress**: Set the remote `wp-content` path (used by Deployer and remote file browsing)
- **Database**: MySQL connection details (used by Database Browser over SSH tunnel)
- **Components**: Plugins/themes/components for requirement checks
- **API**: REST API base URL and authentication (used by module API actions)

Project configurations are stored as JSON files at `~/Library/Application Support/Homeboy/projects/`. You can manage multiple projects and switch between them.

## Installing Modules

1. Go to **Settings > Modules**
2. Click **Install Module from Folder...**
3. Select a folder containing a `module.json` manifest
4. The module appears in the sidebar under "Modules"
5. If setup is required, click the module and follow the prompts

Modules are installed to:
```
~/Library/Application Support/Homeboy/modules/
```

## Server Configuration (SSH)

Remote features (deployments, database tunnel, remote file browsing) require an SSH server.

1. Go to **Settings > Servers**
2. Click **Add Server**
3. Fill in:
   - **Server ID**: unique identifier (e.g. `production-1`)
   - **Display Name**: any friendly name
   - **Host**: SSH host/IP
   - **Username**: SSH user
   - **Port**: usually `22`
4. Under **SSH Key**, click **Generate SSH Key**
5. Click **Show** to copy the public key and add it to the server’s `~/.ssh/authorized_keys`
6. Click **Test SSH Connection** to verify

For WordPress projects, set **WordPress Deployment → wp-content path** in the same tab (you can use **Browse** and then **Validate wp-content**).

## Project Structure

```
Homeboy/
├── App/                          # App entry point
├── Core/
│   ├── API/                      # REST API client
│   ├── Auth/                     # Keychain and authentication
│   ├── CLI/                      # CLI installer
│   ├── Config/                   # JSON configuration management
│   ├── Copyable/                 # Error/warning copy system
│   ├── Database/                 # MySQL and SSH tunnel services
│   ├── Modules/                  # Module loading and execution
│   ├── Process/                  # Python and shell runners
│   └── SSH/                      # SSH/SCP operations
├── Modules/
│   ├── ConfigEditor/             # Configuration editor
│   ├── DatabaseBrowser/          # Database browser
│   ├── DebugLogs/                # Debug log viewer
│   ├── Deployer/                 # Deployment module
│   └── WPCLITerminal/            # WP-CLI terminal
├── ViewModels/                   # Module view models
├── Views/                        # SwiftUI views
│   ├── Components/               # Reusable components
│   ├── Modules/                  # Dynamic module UI
│   └── Settings/                 # Settings tabs
└── docs/                         # Documentation
CLI/
├── main.swift                    # Entry point and Projects command
├── Commands/                     # CLI command implementations
│   ├── WPCommand.swift           # WP-CLI passthrough
│   ├── DBCommand.swift           # Database operations
│   ├── DeployCommand.swift       # Component deployment
│   ├── SSHCommand.swift          # SSH command execution
│   └── ProjectsCommand.swift     # Project listing
└── Utilities/
    └── OutputFormatter.swift     # Table and JSON formatting
```

## Configuration Storage

Site configurations are stored as JSON files:
```
~/Library/Application Support/Homeboy/
├── config.json                   # App-level config
├── projects/                     # Project configurations
│   └── <project-id>.json
├── servers/                      # SSH server configurations
│   └── <server-id>.json
├── modules/                      # Installed modules
│   └── <module-id>/
│       ├── module.json           # Module manifest
│       ├── venv/                 # Python virtual environment
│       └── config.json           # Module settings
├── keys/                         # SSH private/public keys (per server)
│   ├── <server-id>_id_rsa
│   └── <server-id>_id_rsa.pub
└── playwright-browsers/          # Shared Playwright browsers
```

## API Authentication

The app supports JWT authentication with any WordPress site implementing standard auth endpoints:
- Access and refresh tokens stored in macOS Keychain (per-site)
- Auto-refresh before token expiry
- Requires valid credentials for whatever `/auth/login` implementation your WordPress site uses

Configure the API base URL in **Settings > API** for each WordPress site profile. API authentication enables module actions that interact with your WordPress REST API.

## Creating Modules

Modules follow a JSON manifest contract. See [docs/MODULE-SPEC.md](docs/MODULE-SPEC.md) for:
- Manifest schema
- Input types (text, stepper, toggle, select)
- Output display options
- Builtin and API actions
- Script output contract

Example module structure:
```
my-module/
├── module.json       # Manifest (required)
├── script.py         # Python entry point
└── README.md         # Documentation
```

## Development

Regenerate the Xcode project after adding/removing files:
```bash
xcodegen generate
```

## License

This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 2 of the License, or (at your option) any later version.

See [LICENSE](LICENSE) for the full license text.
