# Changelog

All notable code changes to this project are documented in this file.

## 0.3.0

- App: migrate the entire codebase from `ExtraChillDesktop/` to the new `Homeboy/` app structure.
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
