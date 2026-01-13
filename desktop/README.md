# Homeboy

Native macOS SwiftUI application for development and deployment automation. Supports WordPress, Node.js, and custom project types via extensible JSON definitions. Configure multiple sites and extend functionality with installable modules.

**Note:** The desktop app may lag behind the Homeboy CLI. The CLI is the source of truth for behavior and output contracts.

## Features

### Core Tools

**Deployer**
One-click deployment of components (plugins, themes, packages) to any SSH-accessible server.
- SSH key authentication
- Deploys prebuilt artifacts (build happens via the CLI `homeboy build`, not as a deploy flag)
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

Homeboy Desktop shells out to the standalone `homeboy` CLI binary for most core operations.

This README avoids duplicating CLI reference docs.

- Canonical CLI reference: `homeboy docs`
- Desktop/CLI integration notes: [docs/CLI.md](docs/CLI.md)
- Embedded CLI markdown sources: [`../homeboy-core/docs/`](../homeboy-core/docs/index.md)

Compatibility note: the CLI is the source of truth for command behavior and output. The desktop app may lag behind and not support newer commands/options until it is updated.

## Requirements

- macOS 14.4+ (Sonoma)
- Xcode 15.0+
- Python 3.12+ (Homebrew) - for modules with Python scripts

## Setup

This README keeps desktop-specific setup and defers general setup to the root README.

- Canonical monorepo setup: [`../README.md`](../README.md)

### 1. Clone and Open

```bash
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy
```

### 2. Generate Xcode Project + Run

The desktop app uses `project.yml` + XcodeGen.

```bash
xcodegen generate --spec homeboy-desktop/project.yml
open homeboy-desktop/Homeboy.xcodeproj
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

Project configurations are stored under `~/Library/Application Support/homeboy/projects/` (see `docs/CLI.md` for the full shared config tree).

### Project Types

Project type support is primarily defined by installed modules and the CLI; the desktop app UI may not expose all project/module settings.

## Installing Modules

1. Go to **Settings > Modules**
2. Click **Install Module from Folder...**
3. Select a folder containing a `homeboy.json` manifest
4. The module appears in the sidebar under "Modules"
5. If setup is required, click the module and follow the prompts

Modules are installed to:
```
~/Library/Application Support/homeboy/modules/
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

## Documentation map

This README stays desktop-focused and intentionally avoids duplicating CLI reference docs.

- Desktop 1 CLI integration + config tree: [`docs/CLI.md`](docs/CLI.md)
- Embedded CLI markdown sources (canonical command docs): [`../homeboy-core/docs/index.md`](../homeboy-core/docs/index.md)
- Module manifest spec (`homeboy.json`): [`docs/MODULE-SPEC.md`](docs/MODULE-SPEC.md)

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
├── homeboy.json       # Manifest (required)
├── script.py          # Optional script entrypoint
└── README.md          # Documentation
```

## Development

Regenerate the desktop app Xcode project after adding/removing files:
```bash
xcodegen generate --spec homeboy-desktop/project.yml
```

## License

This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 2 of the License, or (at your option) any later version.

See [LICENSE](LICENSE) for the full license text.
