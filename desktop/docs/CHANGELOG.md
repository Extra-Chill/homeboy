# Changelog

All notable code changes to this project are documented in this file.

## 0.7.1

### Improvements
- **Xcode Project**: Regenerated Xcode project with updated CLI integration files.
- **CLIInstaller**: Added back for manual CLI installation fallback.
- **LocalEnvironment**: Added to Process directory for environment type handling.

## 0.7.0

This release completes the CLI/Desktop split. The bundled Swift CLI has been removed; the desktop app now uses a standalone Rust CLI installed via Homebrew.

### Breaking Changes
- **CLI Decoupled**: The bundled Swift CLI is removed. Install the CLI separately via `brew tap extra-chill/tap && brew install homeboy`.
- **Config**: Project configuration uses `localEnvironment`.

### New Features
- **CLIBridge**: Added `CLIBridge.swift` for shelling out to external `homeboy` CLI.
- **CLIVersionChecker**: Checks CLI installation status and updates from GitHub releases (1-hour cache).
- **CLI Setup Sheet**: Non-blocking setup sheet on app launch when CLI is not installed.
- **GeneralSettingsTab CLI Status**: Shows CLI version, installation instructions, and upgrade prompts.

### Removed
- **CLIInstaller**: Removed - CLI installation is now via Homebrew.
- **Bundled Swift CLI**: Removed all files in `CLI/` directory (~4500 lines).

## 0.6.3

This release begins the CLI/Desktop split: the CLI is gaining functionality that can be installed/used independently, while still sharing the same configuration directory with the desktop app.

### New Features
- **Logs CLI Command**: Added `homeboy logs` for remote log file operations (list, show, pin, unpin, clear).
- **Component CLI Command**: Added `homeboy component` for managing shared component configurations across projects.
- **SSH Key Manager**: Added `SSHKeyManager` for per-server SSH key file management with Keychain integration.
- **Artifact Version Parser**: Added `ArtifactVersionParser` for detecting versions from build artifacts (ZIP/TAR headers, package.json, plugin headers).
- **Component Configuration**: Added `ComponentConfiguration` as a standalone config type for sharing component definitions across projects.

### Improvements
- **Server CLI Enhancements**: Added `homeboy server key` subcommand for SSH key generation and management.
- **Deployer Version Tracking**: Deployer now tracks three version sources (source, artifact, remote) with improved comparison logic.
- **Project Configuration Refactor**: Simplified `ProjectConfiguration` by extracting component definitions to `ComponentConfiguration`.

### CLI
- Added `LogsCommand.swift` with list, show, pin, unpin, clear subcommands.
- Added `ComponentCommand.swift` with create, show, set, delete, list subcommands.
- Enhanced `ServerCommand.swift` with SSH key management via `key` subcommand.

## 0.6.2

### Improvements
- **Centralized App Paths**: Added `AppPaths` and updated the app + CLI to use it for Application Support locations (projects, servers, modules, docs, keys, backups).
- **Typed Configuration Change Events**: Replaced NotificationCenter project-change notifications with `ConfigurationObserver` + `ConfigurationChangeType`, and updated observing ViewModels to react only to relevant field changes.
- **Remote Path Normalization**: Added `RemotePathResolver` (plus WordPress-specific helpers) and refactored SSH file/log browsing + module paths to avoid slash-joining edge cases.
- **Non-Blocking SSH Tunnel Startup**: Updated `SSHTunnelService` to avoid `Thread.sleep` during tunnel establishment and stale tunnel cleanup.

### CLI
- **Deploy Is Now Deploy-Only**: `homeboy deploy` no longer runs `build.sh`; it requires an existing build artifact and deploys via `DeploymentService`.
- **Config + Project Type Resolution Cleanup**: CLI commands now use `ConfigurationManager.readProject`/`readServer` and `ProjectTypeManager.shared.resolve(...)` with shared path helpers.

### SSH / Deployment
- **Staged Uploads + Resolver-Based Paths**: `DeploymentService` now stages artifacts under `~/tmp` and uses `RemotePathResolver` for component paths and version-file lookups.

## 0.6.1

### New Features
- **CLI Docs Command**: Added `homeboy docs [topic...]` to display bundled CLI documentation in the terminal, with optional heading-based filtering.
- **Docs Bundling for CLI**: Removed; CLI docs are embedded in the Rust CLI binary.

### Improvements
- **Configuration Reactivity**: Added directory watchers so the app UI updates available projects/servers when CLI edits JSON configs.
- **Database Browser**: Added Command-click multi-select for tables, bulk protect/add-to-group actions, and a “Regenerate Default Groupings” flow.
- **Deployer Version Checks**: Remote version fetching is throttled and reports per-component parse errors instead of failing the whole refresh.
- **Remote Tools**: Remote file editor shows file size; log viewer browsing is now a modal file browser instead of a collapsible sidebar.
- **UI Components**: Added context menu plumbing to `NativeDataTable`, and replaced tab-bar buttons with gesture-based targets to avoid nested button issues.

## 0.6.0

### New Features
- **Module CLI Command**: Added `homeboy module` command for terminal-based module management and execution.
  - `homeboy module list [--project <id>]` - List available modules with optional project compatibility filtering
  - `homeboy module run <module-id> [--project <id>] [args...]` - Run CLI modules locally
- **Generic CLI Runtime Type**: Added `cli` runtime type for modules, enabling project-type-agnostic CLI execution via command templates.

### Removed
- **Legacy `wpcli` Runtime Type**: Removed the `wpcli` runtime type alias. Modules should use `cli` runtime type exclusively.
- **Legacy Module Fields**: Removed `command` and `subcommand` fields from RuntimeConfig. CLI modules use the `args` field for command templates.

### Refactoring
- **LocalEnvironmentConfig**: Renamed `localDev`/`localCLI` to `localEnvironment` in project configuration with updated field names (`sitePath` instead of legacy `wpCliPath`). Migration support was removed; configs must be updated manually.

### CLI
- Added `ModuleCommand.swift` with `ModuleList` and `ModuleRun` commands for CLI module execution.
- Module execution uses project type's command template with variable substitution (`{{sitePath}}`, `{{domain}}`, `{{cliPath}}`, `{{args}}`).

## 0.5.0

### New Features
- **Generic Remote CLI System**: Replaced monolithic `WPCommand.swift` with modular `RemoteCommand.swift` supporting multiple tools (WP-CLI, PM2).
- **Template Rendering**: Added `TemplateRenderer.swift` for flexible command template substitution with `{{variable}}` placeholders.
- **PM2 Command Support**: Added `homeboy pm2 <project> [sub-target] <args...>` for Node.js project process management.
- **CLI Configuration**: Added `CLIConfig` to project type definitions, enabling per-project-type remote CLI tools.
- **Subtarget Support**: Added `subTargets` computed property to `ProjectConfiguration` for generic subtarget targeting (e.g., multisite sites, environments).

### Refactoring
- **Remote CLI Architecture**: Refactored WP-CLI command to use shared `RemoteCLI.execute()` system with template-based command rendering.
- **Project Type Extensions**: Added `CLIConfig` with tool name, display name, and command template fields to `ProjectTypeDefinition`.

### Bug Fixes
- **DeployerViewModel**: Fixed async capture bug where `errorMessage` was incorrectly captured in `MainActor.run` block.
- **CreateProjectSheet**: Improved project ID auto-generation UX - now triggers on name field blur instead of on every keystroke.
- **ProjectSwitcherView**: Same project ID auto-generation improvement as CreateProjectSheet.
- **RemoteFileBrowserView**: Fixed selection logic for `selectPath` mode to properly handle directory selection.
- **ServersSettingsTab**: Improved validation UI to use `InlineErrorView` component for wp-content validation errors.

### Build
- Added `xcodegen generate` step to `build.sh` to ensure project.yml changes are automatically applied to Xcode project before building.

### Configuration
- Updated `nodejs.json`: Added PM2 CLI configuration with `pm2 {{args}} {{projectId}}` template.
- Updated `wordpress.json`: Added WP-CLI configuration with `cd {{appPath}} && wp {{args}} --url={{targetDomain}}` template.

### CLI
- Replaced standalone `WPCommand.swift` (178 lines) with `RemoteCommand.swift` (291 lines) containing generic `RemoteCLI` enum and tool-specific commands.
- Added `loadProjectTypeDefinition()` helper for loading bundled and user-defined project type definitions in CLI context.

## 0.4.0

### New Features
- **Copyable System**: Add error/warning/console copy system with one-click clipboard access. Includes `AppError`, `AppWarning`, `ConsoleOutput` types, `ContentContext` for metadata, `CopyButton` component, and view variants (`ErrorView`, `WarningView`, `InlineErrorView`, `InlineWarningView`).
- **RemoteFileEditor**: New module for editing remote files over SSH with backup support, replacing ConfigEditor.
- **RemoteLogViewer**: New module for viewing/searching remote log files with filtering, replacing DebugLogs.
- **ProjectType System**: Extensible project type definitions via `ProjectTypeManager`. Bundled types (WordPress, Node.js) in `Resources/project-types/`. Each type defines features, default pinned files/logs, and database schema.
- **SSH CLI Command**: Add `homeboy ssh <project> [command]` for interactive shells and single-command execution.
- **DeploymentService**: Dedicated service for SSH deployments with build automation, upload, extraction, and permissions.
- **SchemaResolver**: Database schema resolution for detecting WordPress table prefixes and core table suffixes.

### New Components
- **Grouping System**: `GroupingManager` and `AddToGroupMenu` for managing table groups in Database Browser with project config persistence.
- **NativeDataTable**: Native SwiftUI table system (`NativeDataTable`, `DataTableColumn`, `DataTableConstants`) with sorting, column visibility, and row selection.
- **CreateProjectSheet**: Project creation sheet with type selection and initial configuration.
- **PinnableTabBar**: Tab bar with pinning support for frequently used tables/files.
- **LogContentView**: Log content viewer with syntax highlighting and copy-to-clipboard.
- **CopyableTextView**: Text view with integrated copy button.

### Removed
- **ConfigEditor module**: Removed `Homeboy/Modules/ConfigEditor/` (BackupService, ConfigEditorViewModel, ConfigFile, ConfigEditorView).
- **DebugLogs module**: Removed `Homeboy/Modules/DebugLogs/` (DebugLogsViewModel, DebugLogsView).
- **WP-CLI Terminal module**: Removed `Homeboy/Modules/WPCLITerminal/` (local WP-CLI execution on Local by Flywheel sites). Remote WP-CLI is available via CLI tool (`homeboy wp`).
- **MigrationService**: Removed legacy ExtraChillDesktop migration service.
- **WordPressSiteMap**: Removed in favor of new Database Browser grouping system.

### Refactoring
- **Database Browser**: Complete overhaul with grouping system, native table component, SchemaResolver integration. Major updates to `SiteListView`, `TableDataView`, `QueryEditorView`.
- **Deployer**: Refactored ViewModel and Views with improved version comparison and deployment workflow.
- **Configuration System**: Major updates to `ConfigurationManager`, `ProjectConfiguration`, and `DeployableComponent` with validation, multisite support, and project type integration.
- **SSH Services**: Refactored `RemoteFileBrowser`, `RemoteFileEntry`, and `WordPressSSHModule` for improved file operations.
- **Module System**: Updated `ModuleManager`, `ModuleManifest`, `ModuleRunner`, and `ModuleViewModel`.

### Build
- Switch from ZIP to DMG for macOS distribution (`dist/Homeboy-macOS.dmg`).

### Documentation
- Add `docs/CLI.md` with desktop/CLI integration notes (CLI reference docs now live in the CLI binary via `homeboy docs`).
- Add `docs/ERROR-HANDLING.md` for Copyable system specification.

## 0.3.0

- App: migrate the codebase from `ExtraChillDesktop/` to the `Homeboy/` app structure.
- CLI: add a bundled Swift CLI (`homeboy`) with argument-parser commands for projects, deployments, WP-CLI, and database access.
- Build: replace tracked build artifacts with a reproducible Release packaging flow that outputs `dist/Homeboy-macOS.zip`.
- Config: introduce project/server-first configuration (separate reusable SSH `ServerConfig` from per-project settings).

## 0.2.0

- Database Browser: add a new sidebar tool for browsing remote MySQL databases over SSH tunnel.
- Database Browser: categorize multisite tables by site (plus Network/Other) via `WordPressSiteMap`, including protected table checks for core WordPress tables.
- Database Browser: add table viewer with pagination, multi-row selection, copy-to-clipboard, and single-row deletion (with confirmation).
- Database Browser: add SQL query mode with results table, row selection, and copy-to-clipboard.
- Core/Database: add `SSHTunnelService` port-forwarding (local 3307 -> remote 3306) and `MySQLService` for listing tables/columns/rows, executing queries, and destructive operations.

## 0.1.4

- API: extend `User` model with optional `profileUrl` field.

## 0.1.3

- Deployer: console output can now be copied to clipboard (plus icon-only copy/clear controls).
- Deployer: automatically clears component selections after deploy completion/cancel.
- Deployer: build runner now captures remaining process output after termination to avoid UI thread deadlock.
- Deployer: unzip step now also applies `chmod -R 755` to deployed component directory.
- WP-CLI Terminal: terminal and scraper outputs can now be copied to clipboard (with icon-only controls for copy/clear).
- WP-CLI Terminal: process termination now captures remaining output before clearing readability handlers.

## 0.1.2

- Deployer: after a deployment finishes, remote/local versions are reloaded via `refreshVersions()` (previously only re-fetched remote versions).
- Deployer: upload/extract/cleanup now consistently uses `~/tmp/<component>.zip` instead of `/tmp/<component>.zip`.

## 0.1.1

- Added Deployer: component registry, local/remote version comparison, and SSH/SCP-based build+deploy workflow.
- Added SSH tooling: key generation, Keychain/UserDefaults storage, key restoration to disk, and connection testing.
- Updated WP-CLI tooling: Local by Flywheel PHP/MySQL auto-detection, multisite site targeting via `--url`, and a Scraper Tester subtool.
- Updated API contracts: auth responses include refresh expiry and optional avatar URL; newsletter subscribe now posts to `/newsletter/subscribe` with `source`.
- Updated app icon assets.
