# Agent Instructions (homeboy)

## Project Overview

Native macOS SwiftUI application for development and deployment automation. Project-type agnostic architecture supports WordPress, Node.js, and custom project types via extensible JSON definitions. Configurable site profiles manage multiple installations with an extensible module system for custom automation.

**Platform**: macOS 14.4+ (Sonoma)
**Minimum Xcode**: 15.0+
**Framework**: SwiftUI
**Architecture**: MVVM with ObservableObject ViewModels

## Commands

```bash
# Regenerate Xcode project after adding/removing files or changing project.yml settings
xcodegen generate --spec homeboy-desktop/project.yml

# Open in Xcode
open homeboy-desktop/Homeboy.xcodeproj

# Example: WP-CLI on a local WordPress project
cd /path/to/your/local-wp-site/app/public
wp core version
```

**Important:** When changing version numbers (`MARKETING_VERSION`, `CURRENT_PROJECT_VERSION`) in `project.yml`, run `xcodegen generate` to update the Xcode project, then do a clean build. Otherwise the old version remains baked into the app’s Info.plist.

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
│   ├── CLI/                      # CLI bridge + version checking (CLIBridge.swift, CLIVersionChecker.swift)
│   ├── Config/                   # JSON config, ProjectTypeManager, ProjectTypeDefinition
│   ├── Copyable/                 # Error/warning/output copy system
│   ├── Database/                 # Database tooling (CLI-mediated) and schema helpers
│   ├── Grouping/                 # GroupingManager, ItemGrouping, TableProtectionManager
│   ├── Modules/                  # Module plugin system
│   ├── Process/                  # Python and shell runners
│   └── SSH/                      # SSH/SCP operations, DeploymentService
├── Modules/                      # Built-in core tools
│   ├── DatabaseBrowser/          # Database browser (CLI-backed)
│   ├── Deployer/                 # SSH deployment module
│   ├── RemoteFileEditor/         # Remote file editing over SSH
│   └── RemoteLogViewer/          # Remote log viewing over SSH
├── ViewModels/                   # Module view models
├── Views/
│   ├── Components/               # Reusable UI (Table/, Grouping/, etc.)
│   ├── Modules/                  # Dynamic module UI harness
│   └── Settings/                 # Settings tabs
└── docs/                         # Documentation
(Legacy Swift CLI sources are removed.)

The desktop app shells out to the system-installed `homeboy` binary via:
- `Homeboy/Core/CLI/CLIBridge.swift` (command execution)
- `Homeboy/Core/CLI/CLIVersionChecker.swift` (CLI discovery, installed/latest version checks)

CLI discovery checks these paths in order: `/opt/homebrew/bin/homeboy` (Apple Silicon), `/usr/local/bin/homeboy` (Intel), `~/.cargo/bin/homeboy` (Cargo).
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

## Copyable Error System

The Copyable system provides user-friendly error reporting with one-click copy to clipboard. All errors generate structured markdown with debugging context.

### Key Files
- `Core/Copyable/CopyableContent.swift` - Protocol and ContentType enum
- `Core/Copyable/ContentContext.swift` - Rich metadata for debugging context
- `Core/Copyable/AppError.swift` - Copyable error struct
- `Core/Copyable/AppWarning.swift` - Copyable warning struct
- `Core/Copyable/ConsoleOutput.swift` - Copyable console output
- `Core/Copyable/CopyButton.swift` - SwiftUI copy button component

### View Components
- `Views/Components/ErrorView.swift` - Full-screen error with retry
- `Views/Components/InlineErrorView.swift` - Compact inline error
- `Views/Components/WarningView.swift` - Full-screen warning with action
- `Views/Components/InlineWarningView.swift` - Compact inline warning

### ViewModel Pattern
All ViewModels use `@Published var error: AppError?` for consistent error handling:

```swift
@Published var error: AppError?

// Setting error with source context
error = AppError("Database credentials not configured", source: "Database Browser")

// Setting error with file path
error = AppError(error.localizedDescription, source: "Log Viewer", path: file.displayName)
```

See `docs/ERROR-HANDLING.md` for the complete Copyable system specification.

## Core Tools

### Deployer
SSH/SCP deployment of components (plugins, themes, packages).
- Component registry defined in JSON project config
- Deploys prebuilt artifacts (build is optional via the CLI `--build` flag)
- Version comparison

### Database Browser
Browse remote databases via the `homeboy db` CLI (executed over SSH where applicable).
- Table categorization via SchemaResolver (multisite support for WordPress projects)
- Grouping system for organizing tables
- Query editor with NativeDataTable results
- Row selection and clipboard

### Remote File Editor
Edit remote files over SSH with backup support.
- Pinnable file tabs for frequently accessed files
- Syntax highlighting via CodeTextView

### Remote Log Viewer
View and search remote log files over SSH.
- Real-time log viewing with filtering
- Pinnable log tabs for frequently accessed logs

## CLI Tool

The desktop app uses the decoupled `homeboy` CLI. CLIVersionChecker discovers the CLI at known installation paths (Apple Silicon Homebrew, Intel Homebrew, Cargo) and CLIBridge delegates to it for command execution.

### Key Files
- `Homeboy/Core/CLI/CLIVersionChecker.swift` - CLI discovery, path caching, installed/latest version checks
- `Homeboy/Core/CLI/CLIBridge.swift` - Executes CLI commands and decodes JSON results

Run `homeboy docs` for the canonical CLI documentation.

## Configuration

Configuration is stored as JSON:
```
~/Library/Application Support/Homeboy/
├── config.json           # App-level config (active project ID)
├── projects/             # Per-project configuration
│   └── <project-id>.json
├── servers/              # SSH server configuration
│   └── <server-id>.json
├── modules/              # Installed modules
├── keys/                 # SSH keys (per server)
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

Homeboy does not run an automatic migration from the legacy ExtraChillDesktop app.

- The Keychain service is `com.extrachill.homeboy`.
- `KeychainService.clearLegacyTokens()` removes legacy, non-namespaced token keys (`accessToken`, `refreshToken`) if needed.
