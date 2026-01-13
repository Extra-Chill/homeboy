# Homeboy CLI documentation

This directory contains the markdown docs embedded into the `homeboy` binary and displayed via `homeboy docs`.

Homeboy is a config-driven automation engine for development and deployment automation, with standardized patterns and a stable JSON output envelope for most commands.

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

- `projects/`
- `servers/`
- `components/`
- `modules/`
- `keys/`
- `backups/`

Notes:

- Embedded CLI docs ship inside the binary (see [Embedded docs topic resolution](embedded-docs/embedded-docs-topic-resolution.md)).
- Module docs load from each installed module’s `docs/` folder: `dirs::config_dir()/homeboy/modules/<moduleId>/docs/` (same topic-key rules as core docs).
- The CLI does not write documentation into `<config dir>/homeboy/docs/`.

