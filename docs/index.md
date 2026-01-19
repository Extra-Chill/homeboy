# Homeboy CLI documentation

This directory contains the markdown docs embedded into the `homeboy` binary and displayed via `homeboy docs`.

Homeboy is a config-driven automation engine for development and deployment automation, with standardized patterns and a stable JSON output envelope for most commands.

## CLI

- Root command + global flags: [Root command](cli/homeboy-root-command.md)
- Full built-in command list: [Commands index](commands/commands-index.md)
- Changes summary command: [changes](commands/changes.md)
- JSON output envelope: [JSON output contract](json-output/json-output-contract.md)
- Embedded docs behavior: [Embedded docs topic resolution](embedded-docs/embedded-docs-topic-resolution.md)
- Changelog content: [Changelog](changelog.md)

## Documentation Management

Homeboy provides tooling for AI-assisted documentation generation and maintenance:

- `homeboy docs scaffold <component>` - Analyze codebase and report documentation status
- `homeboy docs audit <component>` - Validate documentation links and detect stale references
- `homeboy docs generate --json` - Bulk create documentation files from JSON spec
- `homeboy docs documentation/index` - Documentation philosophy and principles
- `homeboy docs documentation/alignment` - Instructions for maintaining existing docs
- `homeboy docs documentation/generation` - Instructions for generating new docs
- `homeboy docs documentation/structure` - File organization standards

## Configuration

Configuration and state live under universal directory `~/.config/homeboy/` (all platforms).

- macOS: `~/.config/homeboy/`
- Linux: `~/.config/homeboy/`
- Windows: `%APPDATA%\homeboy\`

Common paths:

- `projects/`
- `servers/`
- `components/`
- `modules/`
- `keys/`
- `backups/`

Notes:

- Embedded CLI docs ship inside the binary (see [Embedded docs topic resolution](embedded-docs/embedded-docs-topic-resolution.md)).
- Module docs load from each installed module’s `docs/` folder under the Homeboy config root: `~/.config/homeboy/modules/<module_id>/docs/` (same topic-key rules as core docs).
- The CLI does not write documentation into `~/.config/homeboy/docs/`.

