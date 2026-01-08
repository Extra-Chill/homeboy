# Agent Instructions (homeboy)

## Project Overview

Native macOS SwiftUI application for WordPress development and deployment automation. Supports configurable site profiles for managing multiple WordPress installations with an extensible module system for custom automation.

**Platform**: macOS 14.4+ (Sonoma)
**Minimum Xcode**: 15.0+
**Framework**: SwiftUI
**Architecture**: MVVM with ObservableObject ViewModels

## Commands

```bash
# Regenerate Xcode project after adding/removing files
xcodegen generate

# Open in Xcode
open Homeboy.xcodeproj

# WP-CLI on local site
cd /path/to/your/local-wp-site/app/public
wp core version
```

## Code Style

- **Swift**: SwiftUI with MVVM pattern
- **Naming**: PascalCase types, camelCase properties/functions
- **Async**: Use async/await for network operations
- **State**: @Published for reactive state, @AppStorage for UserDefaults
- **Formatting**: 4-space indent, Xcode defaults

## Directory Structure

```
Homeboy/
├── App/                          # App entry and root view
├── Core/
│   ├── API/                      # HTTP client and types
│   ├── Auth/                     # Keychain and auth management
│   ├── Config/                   # JSON configuration management
│   ├── Database/                 # MySQL and SSH tunnel services
│   ├── Modules/                  # Module plugin system
│   ├── Process/                  # Python and shell runners
│   └── SSH/                      # SSH/SCP operations
├── Modules/                      # Built-in core tools
│   ├── ConfigEditor/             # Configuration file editor
│   ├── DatabaseBrowser/          # MySQL database browser
│   ├── DebugLogs/                # Debug log viewer
│   ├── Deployer/                 # SSH deployment module
│   └── WPCLITerminal/            # WP-CLI execution
├── ViewModels/                   # Module view models
├── Views/
│   ├── Components/               # Reusable UI components
│   ├── Modules/                  # Dynamic module UI harness
│   └── Settings/                 # Settings tabs
└── docs/                         # Documentation
```

## Module Plugin System

The app supports installable modules via JSON manifest. Modules are stored at:
```
~/Library/Application Support/Homeboy/modules/<module-id>/
```

### Key Files
- `Core/Modules/ModuleManifest.swift` - Codable types for module.json
- `Core/Modules/ModuleManager.swift` - Module discovery and loading
- `Core/Modules/ModuleRunner.swift` - Script execution
- `Core/Modules/ModuleInstaller.swift` - Venv and dependency installation

### Module UI Components
- `Views/Modules/ModuleContainerView.swift` - Main module wrapper
- `Views/Modules/ModuleInputsView.swift` - Dynamic form from manifest
- `Views/Modules/ModuleResultsView.swift` - Dynamic table from output schema
- `Views/Modules/ModuleActionsBar.swift` - Builtin and API actions

See `docs/MODULE-SPEC.md` for the complete module manifest specification.

## Core Tools

### Deployer
SSH/SCP deployment of WordPress plugins and themes.
- Component registry defined in JSON site config
- Build script execution
- Version comparison

### WP-CLI Terminal
Execute WP-CLI commands on Local by Flywheel sites.
- Real-time output streaming
- Command history
- Environment detection

### Database Browser
Browse remote MySQL databases over SSH tunnel.
- WordPress multisite table categorization
- Query editor
- Row selection and clipboard

### Config Editor
Edit JSON configuration files with backup support.

### Debug Logs
View WordPress debug logs from remote servers.

## Configuration

Site configurations stored as JSON:
```
~/Library/Application Support/Homeboy/
├── config.json           # Active site ID
├── sites/                # Per-site configuration
│   └── <site-id>.json
├── modules/              # Installed modules
└── playwright-browsers/  # Shared Playwright cache
```

## API Integration

Supports JWT authentication with WordPress REST APIs.
- Tokens stored in macOS Keychain
- Auto-refresh before expiry
- Configurable base URL per site

### Standard Endpoint Patterns
The app expects WordPress REST APIs to implement these standard auth patterns:
- `POST /auth/login` - Login with identifier/password
- `POST /auth/refresh` - Refresh access token
- `GET /auth/me` - Get current user
- `POST /auth/logout` - Logout

Module-defined API actions can call any endpoint on the configured site.

## Security

- Auth tokens in Keychain (kSecClassGenericPassword)
- Non-sensitive settings in UserDefaults
- No hardcoded secrets
- Per-module isolated Python environments

## Migration

On first launch, Homeboy automatically migrates data from the previous ExtraChillDesktop installation:
- Application Support folder (`~/Library/Application Support/ExtraChillDesktop/` → `Homeboy/`)
- Keychain items (service `com.extrachill.desktop` → `com.extrachill.homeboy`)
