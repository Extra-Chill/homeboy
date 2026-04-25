# Homeboy CLI documentation

This directory contains the markdown docs embedded into the `homeboy` binary and displayed via `homeboy docs`.

Homeboy is a config-driven automation engine for development and deployment automation, with standardized patterns and a stable JSON output envelope for most commands.

## Code Factory

- [Code Factory](code-factory.md) - Automated code maintenance: lint+fix → test+fix → audit+fix → release → deploy

## CLI

- Root command + global flags: [Root command](cli/homeboy-root-command.md)
- Full built-in command list: [Commands index](commands/commands-index.md)
- Code audit (convention drift, structural analysis): [audit](commands/audit.md)
- Changes summary: [changes](commands/changes.md)
- Reproducible local dev environments: [rig](commands/rig.md)
- Rig spec JSON schema: [rig-spec](commands/rig-spec.md)
- Performance benchmarks with baseline ratchet: [bench](commands/bench.md)
- API authentication scoped per project: [auth](commands/auth.md)
- JSON output envelope: [JSON output contract](architecture/output-system.md)
- Embedded docs behavior: [Embedded docs topic resolution](architecture/embedded-docs-topic-resolution.md)
- Changelog content: [Changelog](changelog.md)
- Template variables: [Template variables reference](templates.md)

## Schemas

JSON configuration schemas for components, projects, servers, fleets, and extensions:

- [Component schema](schemas/component-schema.md) - Buildable/deployable units
- [Project schema](schemas/project-schema.md) - Deployable environments
- [Server schema](schemas/server-schema.md) - SSH connection settings
- [Fleet schema](schemas/fleet-schema.md) - Named groups of projects
- [Extension manifest schema](schemas/extension-manifest-schema.md) - Extension configuration

## Architecture

Internal system architecture and internals:

- [Architecture overview](developer-guide/architecture-overview.md) - High-level system design
- [API client system](architecture/api-client.md) - HTTP client and authentication
- [Keychain/secrets management](architecture/keychain-secrets.md) - Secure credential storage
- [SSH key management](architecture/ssh-key-management.md) - SSH key handling
- [Release pipeline system](architecture/release-pipeline.md) - Local release orchestration
- [Execution context](architecture/execution-context.md) - Runtime context for extensions
- [Embedded docs](architecture/embedded-docs-topic-resolution.md) - Documentation system internals

## Developer Guide

Guides for contributing to Homeboy:

- [Architecture overview](developer-guide/architecture-overview.md) - System architecture
- [Config directory structure](developer-guide/config-directory.md) - File organization
- [Error handling patterns](developer-guide/error-handling.md) - Error recovery strategies

## Documentation Management

Homeboy provides tooling for AI-assisted documentation generation and maintenance:

- `homeboy docs audit <component>` - Validate documentation links, detect stale references and gaps
- `homeboy docs map <component>` - Generate machine-optimized codebase map for AI context
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

- ~/.config/homeboy/projects/
- ~/.config/homeboy/servers/
- ~/.config/homeboy/components/
- ~/.config/homeboy/fleets/
- ~/.config/homeboy/extensions/
- ~/.config/homeboy/keys/
- ~/.config/homeboy/backups/

Notes:

- Embedded CLI docs ship inside the binary (see [Embedded docs topic resolution](architecture/embedded-docs-topic-resolution.md)).
- Extension docs load from each installed extension's `docs/` folder under the Homeboy config root: `~/.config/homeboy/extensions/<extension_id>/docs/` (same topic-key rules as core docs).
- The CLI does not write documentation into `~/.config/homeboy/docs/`.


- [Cross-Compilation Guide](cross-compilation.md) - Platform requirements for native binaries
