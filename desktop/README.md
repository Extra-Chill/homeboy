# Homeboy

Native macOS SwiftUI application for WordPress development and deployment automation. Configure multiple WordPress sites and extend functionality with installable modules.

## Features

### Core Tools

**Deployer**
One-click deployment of WordPress plugins and themes to any SSH-accessible server.
- SSH key authentication (RSA 4096-bit)
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
Edit site configuration JSON files with syntax highlighting and backup support.

**Debug Logs**
View and search WordPress debug logs from remote servers.

### Module System

Extend functionality with installable modules. Modules are self-contained plugins with:
- JSON manifest defining inputs, outputs, and actions
- Isolated Python virtual environments
- Dynamic UI generation from manifest
- API action support for WordPress REST endpoints

See [docs/MODULE-SPEC.md](docs/MODULE-SPEC.md) for the complete module specification.

## Requirements

- macOS 14.4+ (Sonoma)
- Xcode 15.0+
- Python 3.12+ (Homebrew) - for modules with Python scripts
- Homebrew packages: `composer`, `wp-cli` (for deployment and WP-CLI features)

## Setup

### 1. Clone and Open

```bash
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy
open Homeboy.xcodeproj
```

### 2. Build and Run

Build and run from Xcode (Cmd+R). The app uses ad-hoc code signing.

### 3. Configure Your WordPress Sites

Homeboy works with **projects** (site profiles). On first launch, configure your project in **Settings**:
- **General**: Project name and local domain
- **Servers**: Add an SSH server (host/user/port) and generate an SSH key
- **WordPress**: Set the remote `wp-content` path (used by Deployer and remote file browsing)
- **Database**: MySQL connection details (used by Database Browser over SSH tunnel)
- **Components**: Plugins/themes/components for requirement checks
- **API**: REST API base URL and authentication (used by module API actions)

Project configurations are stored as JSON files at `~/Library/Application Support/Homeboy/sites/`. You can manage multiple projects and switch between them.

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
   - **Server ID**: unique identifier (e.g. `cloudways-ec`)
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
│   ├── Config/                   # JSON configuration management
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
```

## Configuration Storage

Site configurations are stored as JSON files:
```
~/Library/Application Support/Homeboy/
├── config.json                   # App-level config
├── sites/                        # Site configurations
│   └── <site-id>.json
├── modules/                      # Installed modules
│   └── <module-id>/
│       ├── module.json           # Module manifest
│       ├── venv/                 # Python virtual environment
│       └── config.json           # Module settings
└── playwright-browsers/          # Shared Playwright browsers
```

## API Authentication

The app supports JWT authentication with any WordPress site implementing standard auth endpoints:
- Access and refresh tokens stored in macOS Keychain (per-site)
- Auto-refresh before token expiry
- Requires WordPress user with `manage_options` capability

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

MIT License - see [LICENSE](LICENSE) for details.
