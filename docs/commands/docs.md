# `homeboy docs`

## Synopsis

```sh
homeboy docs [TOPIC]
homeboy docs list
homeboy docs audit <component-id> [--features] [--docs-dir <DIR>]
homeboy docs map <component-id> [--write] [--include-private]
homeboy docs generate --json '<spec>'
homeboy docs generate --from-audit @audit.json
```

## Description

This command renders documentation topics and provides tooling for documentation management.

**Topic display** renders documentation from:
1. Embedded core docs in the CLI binary
2. Installed extension docs under `<config dir>/homeboy/extensions/<extension_id>/docs/`

**Audit** validates documentation links, detects stale references, identifies undocumented features, and flags priority docs that need review.

**Map** generates machine-optimized codebase maps for AI documentation.

**Generate** creates documentation files in bulk from a JSON spec or from audit output.

## Subcommands

### `audit`

Validates documentation against the codebase. Extracts claims (file paths, directory paths) from docs and verifies them against the filesystem. Also detects features in source code and checks whether they're documented.

```sh
homeboy docs audit homeboy
homeboy docs audit data-machine --features
homeboy docs audit /path/to/project --docs-dir documentation
```

**Arguments:**
- `<component-id>`: Component ID or filesystem path to audit (required)

**Options:**
- `--features`: Include full list of all detected features in output (needed for `generate --from-audit`)
- `--docs-dir <DIR>`: Docs directory relative to component root (overrides config, default: `docs`)

**Output fields:**

| Field | Description |
|-------|-------------|
| `broken_references` | File/directory paths in docs that don't exist on disk |
| `undocumented_features` | Source features (structs, enums) with no documentation reference |
| `priority_docs` | Docs whose referenced source files changed since the baseline tag |
| `detected_features` | All features found in source (only with `--features`) |
| `summary` | Counts: `docs_scanned`, `broken_references`, `priority_docs`, `documented_features`, `total_features` |

**Output:**
```json
{
  "success": true,
  "data": {
    "command": "docs.audit",
    "component_id": "homeboy",
    "baseline_ref": "v0.52.1",
    "summary": {
      "docs_scanned": 53,
      "broken_references": 3,
      "priority_docs": 8,
      "documented_features": 90,
      "total_features": 91,
      "unchanged_docs": 45,
      "undocumented_features": 1
    },
    "broken_references": [
      {
        "doc": "commands/refactor.md",
        "line": 95,
        "claim": "directory path `scripts/build/`",
        "confidence": "unclear",
        "action": "Directory 'scripts/build/' no longer exists. Update or remove this reference."
      }
    ],
    "priority_docs": [
      {
        "doc": "commands/docs.md",
        "reason": "6 referenced source file(s) changed since baseline",
        "changed_files_referenced": ["src/commands/docs.rs", "..."],
        "code_examples": 0,
        "action": "Review documentation for accuracy against current implementation."
      }
    ],
    "undocumented_features": [
      {
        "name": "TestMappingConfig",
        "source_file": "src/core/extension/manifest.rs",
        "line": 59
      }
    ]
  }
}
```

**Agent workflow:**
1. Run `homeboy docs audit <component>`
2. Fix `broken_references` — update or remove stale paths
3. Review `priority_docs` — source changed, verify doc accuracy
4. Document `undocumented_features` if they're part of the public API
5. Re-run audit to confirm findings resolved

### `map`

Generates a machine-optimized codebase map by fingerprinting source files and extracting classes, methods, properties, hooks, and inheritance hierarchies.

```sh
# JSON output to stdout
homeboy docs map my-plugin

# Write markdown files to docs directory
homeboy docs map my-plugin --write

# Include protected methods
homeboy docs map my-plugin --include-private

# Custom source directories
homeboy docs map my-plugin --source-dirs src,lib
```

**Arguments:**
- `<component-id>`: Component to analyze (required)

**Options:**
- `--source-dirs <DIRS>`: Source directories to analyze (comma-separated, overrides auto-detection)
- `--include-private`: Include protected methods and internals (default: public API surface only)
- `--write`: Write markdown files to disk instead of JSON to stdout
- `--output-dir <DIR>`: Output directory for markdown files (default: `docs`)

**Auto-detection:** Without `--source-dirs`, the map command looks for conventional directories (`src`, `lib`, `inc`, `app`, `components`, `extensions`, `crates`). Falls back to extension-based file detection if none found.

**Markdown output (--write):** Generates module pages, class hierarchy, hooks summary. Large modules (>30 classes) are split into sub-pages by class name prefix.

### `generate`

Creates or updates documentation files from a JSON spec or from audit output.

**From JSON spec:**
```sh
homeboy docs generate --json '<spec>'
homeboy docs generate @spec.json
homeboy docs generate -  # read from stdin
```

**JSON Spec Format:**
```json
{
  "output_dir": "docs",
  "files": [
    { "path": "engine.md", "content": "Full markdown content here..." },
    { "path": "handlers.md", "title": "Handler System" },
    { "path": "api/auth.md" }
  ]
}
```

**File spec options:**
- `path` (required): Relative path within output_dir
- `content`: Full markdown content to write
- `title`: Creates file with `# {title}\n` (used if no content)
- Neither: Uses filename converted to title case; infers section headings from sibling docs

**From audit output:**
```sh
homeboy docs audit my-plugin --features > audit.json
homeboy docs generate --from-audit @audit.json
homeboy docs generate --from-audit @audit.json --dry-run
```

Generates reference documentation from detected features, grouped by extension-configured labels and written to configured doc targets.

**Options:**
- `--dry-run`: Show what would be generated without writing files

## Topic Display

### Default (render topic)

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

1. **Audit**: `homeboy docs audit <component>` — find broken refs, stale docs, undocumented features
2. **Learn**: `homeboy docs documentation/generation` — read guidelines
3. **Map**: `homeboy docs map <component>` — generate codebase map for AI context
4. **Generate**: `homeboy docs generate --from-audit @audit.json` — bulk create from audit data
5. **Maintain**: `homeboy docs documentation/alignment` — keep docs current

## Errors

If a topic does not exist, the command fails with an error indicating the topic was not found.

If a component does not exist (for audit/map), the command fails with a component not found error.

## Related

- [audit](audit.md) — code-level convention auditing (different from docs audit)
- [changelog](changelog.md)
- [JSON output contract](../architecture/output-system.md)
