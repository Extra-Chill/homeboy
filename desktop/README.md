# Homeboy

Native macOS SwiftUI application for development and deployment automation. Supports WordPress, Node.js, and custom project types via extensible JSON definitions. Configure multiple sites and extend functionality with installable modules.

## Features

### Core Tools

**Deployer**
One-click deployment of components (plugins, themes, packages) to any SSH-accessible server.
- SSH key authentication
- Build automation via `build.sh` scripts
- Version comparison (local vs remote)
- Per-site component registry

**Database Browser**
Browse and query remote databases via the `homeboy db` CLI (executed over SSH where applicable).
- Table listing with multisite support (WordPress projects)
- Table grouping system for organization
- Query editor with native table results display
- Row selection and clipboard operations

**Remote File Editor**
Edit remote files over SSH with backup support.
- Pinnable file tabs for frequently accessed files
- Syntax highlighting

**Remote Log Viewer**
View and search remote log files over SSH.
- Real-time log viewing with filtering
- Pinnable log tabs for frequently accessed logs

### Module System

Extend functionality with installable modules. Modules are self-contained plugins with:
- JSON manifest defining inputs, outputs, and actions
- Isolated Python virtual environments
- Dynamic UI generation from manifest
- API action support for WordPress REST endpoints

See docs/MODULE-SPEC.md for the complete module specification.

### Command Line Tool

Homeboy includes a CLI (`homeboy`) for terminal access to project management, WordPress operations, database queries, and deployments.

**Installation**: The app prompts to install the CLI on first launch, or install manually via **Settings > General**.

**Available Commands**:
```bash
homeboy projects                                    # List configured projects
homeboy project create "My Site" --type wordpress  # Create a new project (--type required)
homeboy project show extrachill                    # Show project configuration
homeboy server create "Prod" --host server.example.com --user deploy
homeboy wp extrachill plugin list                  # Execute WP-CLI on production
homeboy pm2 api-server list                        # Execute PM2 on Node.js servers
homeboy db extrachill tables                       # List database tables
homeboy deploy extrachill --all                    # Deploy all components
homeboy ssh extrachill                             # Open interactive SSH shell
homeboy module list                                # List available modules
homeboy module run <module-id>                     # Run a CLI module locally
```

Projects must be configured via Homeboy.app before using the CLI. See docs/CLI.md for full documentation.

## Requirements

- macOS 14.4+ (Sonoma)
- Xcode 15.0+
- Python 3.12+ (Homebrew) - for modules with Python scripts

## Setup

### 1. Clone and Open

```bash
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy
open Homeboy.xcodeproj
```

### 2. Generate Project + Run

This repo uses `project.yml` + XcodeGen.

```bash
xcodegen generate
open Homeboy.xcodeproj
```

Build and run from Xcode (Cmd+R).

### 3. Configure Your Projects

Homeboy works with **projects** (site profiles). On first launch, configure your project in **Settings**:
- **General**: Project name, type, and local domain
- **Servers**: Add an SSH server (host/user/port) and generate an SSH key
- **Project Settings**: Set the remote base path (used by Deployer and remote file browsing)
- **Database**: Database connection details (used by the Database Browser via the CLI)
- **Components**: Plugins, themes, or packages for deployment
- **API**: REST API base URL and authentication (used by module API actions)

Project configurations are stored as JSON files at `~/Library/Application Support/Homeboy/projects/`. You can manage multiple projects and switch between them.

### Project Types

Homeboy ships with built-in definitions for WordPress and Node.js projects. Custom project types can be added via JSON files in `~/Library/Application Support/Homeboy/project-types/`.

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

Remote features (deployments, remote file browsing, remote database access) require an SSH server.

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
6. Click **Test SSH Connection** to verify (requires selecting a project linked to this server)

For WordPress projects, set the **wp-content path** in project settings (you can use **Browse** to discover installations on the server).

## Project Structure

```
Homeboy/
├── App/                          # App entry point
├── Core/
│   ├── API/                      # REST API client
│   ├── Auth/                     # Keychain and authentication
│   ├── CLI/                      # CLI installer
│   ├── Config/                   # JSON config, ProjectTypeManager
│   ├── Copyable/                 # Error/warning copy system
│   ├── Database/                 # Database tooling (CLI-mediated) and schema helpers
│   ├── Grouping/                 # GroupingManager, ItemGrouping
│   ├── Modules/                  # Module loading and execution
│   ├── Process/                  # Python and shell runners
│   └── SSH/                      # SSH/SCP operations, DeploymentService
├── Modules/
│   ├── DatabaseBrowser/          # Database browser
│   ├── Deployer/                 # Deployment module
│   ├── RemoteFileEditor/         # Remote file editor
│   └── RemoteLogViewer/          # Remote log viewer
├── ViewModels/                   # Module view models
├── Views/                        # SwiftUI views
│   ├── Components/               # Reusable components (Table/, Grouping/)
│   ├── Modules/                  # Dynamic module UI
│   └── Settings/                 # Settings tabs
└── docs/                         # Documentation
CLI/
├── main.swift                    # Entry point and Projects command
├── Commands/                     # CLI command implementations
│   ├── DBCommand.swift           # Database operations
│   ├── DeployCommand.swift       # Component deployment
│   ├── ModuleCommand.swift       # Module listing and execution
│   ├── ProjectCommand.swift      # Project CRUD and management
│   ├── ProjectsCommand.swift     # Project listing
│   ├── RemoteCommand.swift       # WP-CLI and PM2 passthrough
│   ├── ServerCommand.swift       # Server configuration
│   └── SSHCommand.swift          # SSH command execution
└── Utilities/
    ├── OutputFormatter.swift     # Table and JSON formatting
    └── TemplateRenderer.swift    # Command template rendering
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

The app supports JWT authentication with REST APIs:
- Access and refresh tokens stored in macOS Keychain (per-site)
- Auto-refresh before token expiry
- Requires valid credentials for the configured `/auth/login` endpoint

Configure the API base URL in **Settings > API** for each project. API authentication enables module actions that interact with your REST API.

## Creating Modules

Modules follow a JSON manifest contract. See docs/MODULE-SPEC.md for:
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
