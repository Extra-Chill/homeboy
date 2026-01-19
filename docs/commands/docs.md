# `homeboy docs`

## Synopsis

```sh
homeboy docs [TOPIC]
homeboy docs list
homeboy docs scaffold <component-id> [--docs-dir <dir>]
homeboy docs audit <component-id>
homeboy docs generate --json '<spec>'
homeboy docs generate @spec.json
homeboy docs generate -
```

## Description

This command renders documentation topics and provides tooling for documentation management.

**Topic display** renders documentation from:
1. Embedded core docs in the CLI binary
2. Installed module docs under `<config dir>/homeboy/modules/<module_id>/docs/`

**Scaffold** analyzes a component's codebase and reports documentation status (read-only).

**Audit** validates documentation links and detects stale references.

**Generate** creates documentation files in bulk from a JSON spec.

## Subcommands

### `scaffold`

Analyzes a component's codebase and reports:
- Source directories found
- Existing documentation files
- Potentially undocumented areas

This is read-only - no files are created. Use the analysis to inform documentation planning.

```sh
homeboy docs scaffold homeboy
homeboy docs scaffold extrachill-api --docs-dir documentation
```

**Arguments:**
- `<component-id>`: Component to analyze (required)

**Options:**
- `--docs-dir <dir>`: Documentation directory to scan (default: `docs`)

**Output:**
```json
{
  "success": true,
  "data": {
    "command": "docs.scaffold",
    "analysis": {
      "component_id": "homeboy",
      "source_directories": ["src", "src/api", "src/models"],
      "existing_docs": ["overview.md", "core-system/engine.md"],
      "undocumented": ["src/api", "src/models"]
    },
    "instructions": "Run `homeboy docs documentation/generation` for writing guidelines",
    "hints": ["Found 3 source directories", "2 docs already exist"]
  }
}
```

### `audit`

Validates documentation for a component by checking:
- **Link validation**: Verifies markdown links resolve to existing docs
- **Path validation**: Checks file path references exist in the component
- **Staleness detection**: Identifies docs that may need review based on recent code changes

```sh
homeboy docs audit homeboy
homeboy docs audit extrachill-api
```

**Arguments:**
- `<component-id>`: Component to audit (required)

**Output:**
```json
{
  "success": true,
  "data": {
    "command": "docs.audit",
    "component_id": "homeboy",
    "summary": {
      "docs_audited": 28,
      "issues_found": 5,
      "stale_docs": 2,
      "broken_links": 3
    },
    "issues": [
      {
        "doc": "commands/deploy.md",
        "issue_type": "broken_link",
        "detail": "Link to 'nonexistent.md' does not resolve",
        "line": 23
      },
      {
        "doc": "core/engine.md",
        "issue_type": "broken_path",
        "detail": "Referenced file 'src/old/removed.rs' does not exist",
        "line": 45
      }
    ],
    "hints": ["2 docs may need review", "3 broken links should be fixed"]
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

1. **Analyze**: `homeboy docs scaffold <component>` - understand current state
2. **Learn**: `homeboy docs documentation/generation` - read guidelines
3. **Plan**: AI determines structure based on analysis + guidelines
4. **Generate**: `homeboy docs generate --json '<spec>'` - bulk create files
5. **Validate**: `homeboy docs audit <component>` - check for broken links and stale docs
6. **Maintain**: `homeboy docs documentation/alignment` - keep docs current

## Errors

If a topic does not exist, the command fails with an error indicating the topic was not found.

If a component does not exist (for scaffold/audit), the command fails with a component not found error.

## Related

- [Changelog command](changelog.md)
- [JSON output contract](../json-output/json-output-contract.md)
