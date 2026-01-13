# Changelog

All notable changes to Homeboy CLI are documented in this file.

(This file is embedded into the CLI binary and is also viewable via `homeboy changelog`.)

## Unreleased

## 0.6.0

- Add universal --merge flag for component/project/server set commands
- Fix changelog entry spacing to preserve blank line before next version
- Refactor core into a headless/public API; treat the CLI as one interface
- Move business logic into homeboy-core and reduce CLI responsibilities
- Standardize command/output layers and keep TTY concerns in the CLI
- Introduce/expand the module system and module settings
- Add generic auth support plus a generic API client/command
- Remove/adjust doctor and error commands during stabilization

## 0.5.0

- Refactor deploy to use a generic core implementation
- Replace component isNetwork flag with extractCommand for post-upload extraction
- Unify module runtime config around runCommand/setupCommand/readyCheck/env and remove plugin-specific fields
- Update docs and examples for new generic deployment and module behavior

## 0.4.1

- Rename plugin terminology to module across CLI/docs
- Remove active project concept; require explicit --project where needed
- Update module manifest filename to homeboy.json

## 0.4.0

- Unify plugins and modules under a single module manifest and config surface
- Remove plugin command and plugin manifest subsystem; migrate CLI/db/deploy/version/build to module-based lookups
- Rename config fields: plugins→modules, plugin_settings→module_settings, modules→scoped_modules

## 0.3.0

- Add plugin support (nodejs/wordpress)
- Add plugin command and plugin manifest integration
- Improve deploy/build/version command behavior and outputs

## 0.2.19

- Fix inverted version validation condition to prevent gaps instead of blocking valid bumps

## 0.2.18

- Fix shell argument escaping for wp and pm2 commands with special characters
- Centralize shell escaping in shell.rs module with quote_arg, quote_args, quote_path functions
- Fix unescaped file paths in logs and file commands
- Remove redundant escaping functions from template.rs, ssh/client.rs, and deploy.rs

## 0.2.17

- Add project set --component-ids to replace component attachments
- Add project components add/remove/clear subcommands
- Add tests for project component attachment workflows

## 0.2.15

- Derive git tag name
- Internal refactor

## 0.2.14

- Fix unused imports warnings

## 0.2.13

- Project rewrite
- Internal cleanup

## 0.2.12

- Refactor command implementations to reduce boilerplate
- Add new CLI flags support
- Fix changelog formatting

## 0.2.10

- Clean up version show JSON output

## 0.2.9

- Fix clippy warnings (argument bundling, test module ordering)

## 0.2.8

- docs: homeboy docs outputs raw markdown by default
- changelog: homeboy changelog outputs raw markdown (removed show subcommand)

## 0.2.7

- Default JSON output envelope; allow interactive passthrough
- Require stdin+stdout TTY for interactive passthrough commands
- Standardize `--json` input spec handling for subcommands that support it (`project create --json`, `changelog --json`)
- Fix changelog finalization formatting

## 0.2.5

- added overlooked config command back in
- docs updated
- module standardized data contract

## 0.2.4

- Restore 'homeboy config' command wiring
- Update command docs to include config

## 0.2.3

- Fix changelog finalize placing ## Unreleased at top instead of between versions
- Fix changelog item insertion removing extra blank lines between items

## 0.2.2

- Add scan_json_dir<T>() helper to json module for directory scanning
- Refactor config list functions to use centralized json helpers
- Refactor module loading to use read_json_file_typed()
- Internal refactor

## 0.2.1

- Default app config values are serialized (no more Option-based defaults for DB settings)
- DB commands now read default CLI path/host/port from AppConfig instead of resolve helpers

## 0.2.0

### Improvements
- **Config schema**: Introduce `homeboy config` command group + `ConfigKeys` schema listing to standardize how config keys are described/exposed.
- **Config records**: Standardize config identity via `slugify_id()` + `SlugIdentifiable::slug_id()` and enforce id/name consistency in `ConfigManager::save_server()` and `ConfigManager::save_component()`.
- **App config**: Extend `AppConfig` with `installedModules: HashMap<String, InstalledModuleConfig>`; each module stores `settings: HashMap<String, Value>` and optional `sourceUrl`.
- **Module scoping**: Add `ModuleScope::{effective_settings, validate_project_compatibility, resolve_component_scope}` to merge settings across app/project/component and validate `ModuleManifest.requires` (for example: `components`).
- **Module execution**: Tighten `homeboy module run` to require an installed/configured entry and resolve project/component context when CLI templates reference project variables.
- **Command context**: Refactor SSH/base-path resolution to shared context helpers (used by `db`/`deploy`) for more consistent configuration errors.
- **Docs**: Normalize docs placeholders (`<projectId>`, `<serverId>`, `<componentId>`) across embedded CLI documentation.

## 0.1.13

### Improvements
- **Changelog**: `homeboy changelog add` auto-detects changelog path when `changelogTargets` is not configured.
- **Changelog**: Default next section label is `Unreleased` (aliases include `[Unreleased]`).
- **Version**: `homeboy version bump` finalizes the "next" section into the new version section whenever `--changelog-add` is used.

## 0.1.12

### Improvements
- **Changelog**: Promote `homeboy changelog` from a shortcut to a subcommand group with `show` and `add`.
- **Changelog**: Add `homeboy changelog add <componentId> <message>` to append items to the “next” section (defaults to `Unreleased`).
- **Changelog**: Auto-detect changelog path (`CHANGELOG.md` or `docs/changelog.md`) when `changelogTargets` is not configured.
- **Config**: Support `changelogTargets` + `changelogNextSectionLabel`/`changelogNextSectionAliases` at component/project/app levels.
- **Version**: Write JSON version bumps via the `version` key (pretty-printed) when using the default JSON version pattern.
- **Deploy**: Load components via `ConfigManager` instead of ad-hoc JSON parsing.

## 0.1.11

### Improvements
- **Docs**: Expanded `docs/index.md` to include configuration/state directory layout and a clearer documentation index.
- **Docs/Positioning**: Refined README messaging to emphasize Homeboy’s LLM-first focus.

## 0.1.10

### Improvements
- **Modules**: Added git-based module workflows: `homeboy module install`, `homeboy module update`, and `homeboy module uninstall`.
- **Modules**: Added `.install.json` metadata (stored inside each module directory) to enable reliable updates from the original source.
- **Docs/Positioning**: Updated README and docs index to reflect LLM-first focus and Homeboy data directory layout.

## 0.1.9

### Improvements
- **Project management**: Added `homeboy project list` and `homeboy project pin` subcommands to manage pinned files/logs per project.
- **Config correctness**: Project configs are a strict `ProjectRecord` (`id` derived via `slugify_id(name)`) with validation to prevent mismatched IDs and to clear `active_project_id` when a project is deleted.
- **Docs**: Updated embedded docs to reflect new/removed commands.

## 0.1.8

### Improvements
- **Versioning**: `versionTargets` are now first-class for component version management (supports multiple files and multiple matches per file, with strict validation).
- **Deploy**: Reads the component version from `versionTargets[0]` for local/remote comparisons.

## 0.1.7

### Improvements
- **Component configuration**: Support `versionTargets` (multiple version targets) and optional `buildCommand` in component config.
- **Version bumping**: `homeboy version bump` validates that all matches in each target are the same version before replacing.
- **Deploy JSON output**: Deploy results include `artifactPath`, `remotePath`, `buildCommand`, `buildExitCode`, and an upload exit code for clearer automation.
- **Docs refresh**: Updated command docs + JSON output contract; removed outdated command/contract doc.

## 0.1.6

### New Features
- **Embedded docs**: Embed `homeboy-core/docs/**/*.md` into the CLI binary at build time, so `homeboy docs` works in Homebrew/releases.
- **Changelog command**: Added `homeboy changelog` shortcut (equivalent to `homeboy docs changelog`).

### Improvements
- **Docs source of truth**: Keep CLI documentation under `homeboy-core/docs/` and embed it into the CLI binary.
- **Docs topic listing**: `available_topics` is now generated dynamically from embedded keys (newline-separated).

## 0.1.5

### Breaking Changes
- **Docs Command Output**: `homeboy docs` now prints embedded markdown to stdout by default (instead of paging).

### New Features
- **Core Path Utilities**: Added `homeboy_core::base_path` helpers for base path validation and remote path joining (`join_remote_path`, `join_remote_child`, `remote_dirname`).
- **Core Shell Utilities**: Added `homeboy_core::shell::cd_and()` to build safe "cd && <cmd>" strings.
- **Core Token Utilities**: Added `homeboy_core::token` helpers for case-insensitive identifiers and doc topic normalization.

### Improvements
- **Unified JSON Output**: CLI commands now return typed structs and are serialized in `crates/homeboy/src/main.rs`, standardizing success/error output and exit codes.
- **Docs & Skill Updates**: Updated documentation and the Homeboy skill.

## 0.1.4

### New Features
- **Build Command**: New `homeboy build <component>` for component-scoped builds
  - Runs a component build in its `local_path`

### Improvements
- **Version Utilities**: Refactored version parsing to shared `homeboy-core` library
  - `parse_version`, `default_pattern_for_file`, `increment_version` now in core
  - Enables future reuse across CLI components

## 0.1.3

### New Features
- **Version Command**: New `homeboy version` command for component-scoped version management
  - `show` - Display current version from component's version_file
  - `bump` - Increment version (patch/minor/major) and write back to file
  - Auto-detects patterns for .toml, .json, .php files

## 0.1.2

### New Features
- **Git Command**: New `homeboy git` command for component-scoped git operations
  - `status` - Show git status for a component
  - `commit` - Stage all changes and commit with message
  - `push` - Push local commits to remote (with `--tags` flag support)
  - `pull` - Pull remote changes
  - `tag` - Create git tags (lightweight or annotated with `-m`)

### Improvements
- **Dogfooding Support**: Homeboy can now manage its own releases via git commands

## 0.1.1

### Breaking Changes
- **Config Rename**: `local_cli` renamed to `local_environment` in project configuration JSON files (matches desktop app 0.7.0).

### Improvements
- **Deploy Command**: Improved deployment workflow.
- **Module Command**: Enhanced CLI module execution with better variable substitution.
- **PM2 Command**: Improved PM2 command handling for Node.js projects.
- **WP Command**: Improved WP-CLI command handling for WordPress projects.

## 0.1.0

Initial release.
- Project, server, and component management
- Remote SSH operations (wp, pm2, ssh, db, file, logs)
- Deploy and pin commands
- CLI module execution
- Shared configuration with desktop app
