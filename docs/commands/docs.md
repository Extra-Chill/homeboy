# `homeboy docs`

## Synopsis

```sh
homeboy docs [TOPIC]...
homeboy docs list
homeboy docs scaffold [--source <dir>] [--docs-dir <dir>]
homeboy docs generate --json '<spec>'
homeboy docs generate @spec.json
homeboy docs generate -
```

## Description

This command renders documentation topics and provides tooling for documentation management.

**Topic display** renders documentation from:
1. Embedded core docs in the CLI binary
2. Installed module docs under `<config dir>/homeboy/modules/<module_id>/docs/`

**Scaffold** analyzes a codebase and reports documentation status (read-only).

**Generate** creates documentation files in bulk from a JSON spec.

## Subcommands

### `scaffold`

Analyzes the codebase and reports:
- Source directories found
- Existing documentation files
- Potentially undocumented areas

This is read-only - no files are created. Use the analysis to inform documentation planning.

```sh
homeboy docs scaffold
homeboy docs scaffold --source ./my-project --docs-dir documentation
```

**Options:**
- `--source <dir>`: Source directory to analyze (default: current directory)
- `--docs-dir <dir>`: Documentation directory to scan (default: `docs`)

**Output:**
```json
{
  "success": true,
  "data": {
    "command": "docs.scaffold",
    "analysis": {
      "source_directories": ["src", "src/api", "src/models"],
      "existing_docs": ["overview.md", "core-system/engine.md"],
      "undocumented": ["src/api", "src/models"]
    },
    "instructions": "Run `homeboy docs documentation/generation` for writing guidelines",
    "hints": ["Found 3 source directories", "2 docs already exist"]
  }
}
```

### `generate`

Creates or updates documentation files from a JSON spec. Supports bulk creation with optional content.

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
- Neither: Uses filename converted to title case

**Output:**
```json
{
  "success": true,
  "data": {
    "command": "docs.generate",
    "files_created": ["docs/core-system/engine.md", "docs/core-system/handlers.md"],
    "files_updated": [],
    "hints": ["Created 2 files"]
  }
}
```

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

1. **Analyze**: `homeboy docs scaffold` - understand current state
2. **Learn**: `homeboy docs documentation/generation` - read guidelines
3. **Plan**: AI determines structure based on analysis + guidelines
4. **Generate**: `homeboy docs generate --json '<spec>'` - bulk create files
5. **Maintain**: `homeboy docs documentation/alignment` - keep docs current

## Errors

If a topic does not exist, the command fails with:
- `config_missing_key("docs.<topic>")`

## Related

- [Changelog command](changelog.md)
- [JSON output contract](../json-output/json-output-contract.md)
