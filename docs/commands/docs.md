# `homeboy docs`

## Synopsis

```sh
homeboy docs [OPTIONS] [TOPIC] [COMMAND]
homeboy docs list
homeboy docs map [OPTIONS] <component-id>
```

## Description

This command renders embedded documentation topics and provides a codebase-map helper for AI-assisted documentation work.

**Topic display** renders documentation from:

1. Embedded core docs in the CLI binary
2. Installed extension docs under `<config dir>/homeboy/extensions/<extension_id>/docs/`

**Map** generates machine-optimized codebase maps for AI documentation.

## Subcommands

The command has one documentation-management subcommand:

- `map` — generate a machine-optimized codebase map for AI documentation

`help` is also available as the standard CLI help subcommand.

## Options

- `--output <PATH>`: Write structured JSON output to a file in addition to stdout

### `map`

Generates a machine-optimized codebase map by fingerprinting source files and extracting classes, methods, properties, hooks, and inheritance hierarchies.

```sh
# JSON output to stdout
homeboy docs map my-plugin

# Write markdown files to a docs directory
homeboy docs map my-plugin --write

# Include protected methods
homeboy docs map my-plugin --include-private

# Custom source directories
homeboy docs map my-plugin --source-dirs src,lib
```

**Arguments:**

- `<component-id>`: Component to analyze (required)

**Options:**
- `--output <PATH>`: Write structured JSON output to a file in addition to stdout
- `--source-dirs <DIRS>`: Source directories to analyze (comma-separated, overrides auto-detection)
- `--include-private`: Include protected methods and internals (default: public API surface only)
- `--write`: Write markdown files to disk instead of JSON to stdout
- `--output-dir <DIR>`: Output directory for markdown files (default: `docs`)

**Agent workflow:**
1. Run `homeboy docs map <component>` to gather source structure for documentation work.
2. Read the relevant embedded guidance topic, such as `homeboy docs documentation/alignment` or `homeboy docs documentation/generation`.
3. Edit documentation manually against the current source.
4. Use focused source checks, `homeboy audit`, and `homeboy lint` as appropriate for the repository.

**Auto-detection:** Without `--source-dirs`, the map command looks for conventional directories (`src`, `lib`, `inc`, `app`, `components`, `extensions`, `crates`). Falls back to extension-based file detection if none found.

**Markdown output (`--write`):** Generates module pages, class hierarchy, and hooks summary. Large modules (>30 classes) are split into sub-pages by class name prefix.

## Topic Display

### Default Topic Rendering

`homeboy docs <topic>` prints the resolved markdown content to stdout.

```sh
homeboy docs commands/deploy
homeboy docs documentation/generation
```

### `list`

`homeboy docs list` prints available topics as newline-delimited plain text.

## Documentation Topics

Homeboy includes embedded documentation for AI agents:

- `homeboy docs documentation/index` - Documentation philosophy and overview
- `homeboy docs documentation/alignment` - Instructions for aligning existing docs with code
- `homeboy docs documentation/generation` - Instructions for generating new documentation
- `homeboy docs documentation/structure` - File organization and naming patterns

## Workflow

Typical documentation workflow using these commands:

1. **Learn**: `homeboy docs documentation/generation` — read guidelines
2. **Map**: `homeboy docs map <component>` — generate codebase map for AI context
3. **Maintain**: `homeboy docs documentation/alignment` — keep docs current
4. **Verify**: run focused source checks plus `homeboy audit` or `homeboy lint` when those commands cover the changed files

## Errors

If a topic does not exist, the command fails with an error indicating the topic was not found.

If a component does not exist for `map`, the command fails with a component not found error.

## Related

- [audit](audit.md) — code-level convention auditing, including documentation-reference findings when enabled by the audit implementation
- [commands index](commands-index.md)
- [audit](audit.md) — code-level convention auditing, including documentation-reference findings when enabled by the audit implementation
- [changelog](changelog.md)
- [JSON output contract](../architecture/output-system.md)
