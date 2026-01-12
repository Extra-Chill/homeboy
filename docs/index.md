# Homeboy CLI documentation

This directory contains the markdown docs embedded into the `homeboy` binary and displayed via `homeboy docs`.

Homeboy is a config-driven automation engine for repetitive discovery and AI coding workflows, with standardized patterns and a reliable JSON output envelope.

## CLI

- Root command + global flags: [Root command](cli/homeboy-root-command.md)
- Full command list: [Commands index](commands/commands-index.md)
- JSON output envelope: [JSON output contract](json-output/json-output-contract.md)
- Embedded docs behavior: [Embedded docs topic resolution](embedded-docs/embedded-docs-topic-resolution.md)
- Changelog content: [Changelog](changelog.md)
## Configuration

Configuration and state live under the OS config directory (`dirs::config_dir()/homeboy/`):

- macOS: `~/Library/Application Support/homeboy/`
- Linux: `$XDG_CONFIG_HOME/homeboy/` (fallback: `~/.config/homeboy/`)
- Windows: `%APPDATA%\\homeboy\\`

Common paths:

- `homeboy.json`
- `projects/`
- `servers/`
- `components/`
- `modules/`
- `keys/`
- `backups/`

Additional:

- `playwright-browsers/` (used when running Playwright-backed modules; see `PLAYWRIGHT_BROWSERS_PATH`)
- `docs/` (reserved for cached/generated docs)

