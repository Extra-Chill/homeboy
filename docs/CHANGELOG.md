# Changelog

All notable changes to Homeboy CLI are documented in this file.

## 0.1.4

### New Features
- **Build Command**: New `homeboy build <component>` for component-scoped builds
  - Runs component's configured `build_command` in its `local_path`
  - JSON output support with `--json` flag

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
