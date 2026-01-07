# Changelog

All notable code changes to this project are documented in this file.

## 0.1.4

- Bandcamp Scraper: add concurrent worker support (persisted via Settings) and pass `--workers` to the Python runner.
- Bandcamp Scraper: add headless mode flag (`--headless true`) by default.
- Bandcamp Scraper (Python): add stricter email extraction/validation, mailto parsing, domain filtering, external contact-page probing, and rate-limit backoff handling.
- API: extend `User` model with optional `profileUrl` field.

## 0.1.3

- Bandcamp Scraper: console output can now be copied to clipboard (plus icon-only copy/clear controls).
- Cloudways Deployer: console output can now be copied to clipboard (plus icon-only copy/clear controls).
- Cloudways Deployer: automatically clears component selections after deploy completion/cancel.
- Cloudways Deployer: build runner now captures remaining process output after termination to avoid UI thread deadlock.
- Cloudways Deployer: unzip step now also applies `chmod -R 755` to deployed component directory.
- WP-CLI Terminal: terminal and scraper outputs can now be copied to clipboard (with icon-only controls for copy/clear).
- WP-CLI Terminal: process termination now captures remaining output before clearing readability handlers.

## 0.1.2

- Cloudways Deployer: after a deployment finishes, remote/local versions are reloaded via `refreshVersions()` (previously only re-fetched remote versions).
- Cloudways Deployer: upload/extract/cleanup now consistently uses `~/tmp/<component>.zip` instead of `/tmp/<component>.zip`.

## 0.1.1

- Added Cloudways deployment module: component registry, local/remote version comparison, and SSH/SCP-based build+deploy workflow.
- Added SSH tooling: key generation, Keychain/UserDefaults storage, key restoration to disk, and connection testing.
- Updated WP-CLI tooling: Local by Flywheel PHP/MySQL auto-detection, multisite site targeting via `--url`, and a Scraper Tester subtool.
- Updated Bandcamp Scraper: automatic Python venv/dependency setup in Application Support when missing.
- Updated API contracts: auth responses include refresh expiry and optional avatar URL; newsletter subscribe now posts to `/newsletter/subscribe` with `source`.
- Updated app icon assets.
