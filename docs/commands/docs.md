# `homeboy docs`

## Synopsis

```sh
homeboy docs [TOPIC]
homeboy docs list
homeboy docs audit <component-id>
homeboy docs map <component-id>
homeboy docs generate --json '<spec>'
homeboy docs generate @spec.json
homeboy docs generate -
```

## Description

This command renders documentation topics and provides tooling for documentation management.

**Topic display** renders documentation from:
1. Embedded core docs in the CLI binary
2. Installed extension docs under `<config dir>/homeboy/extensions/<extension_id>/docs/`

**Audit** validates documentation links, detects stale references, and identifies undocumented features.

**Map** generates machine-optimized codebase maps for AI documentation.

**Generate** creates documentation files in bulk from a JSON spec.

## Subcommands

### `audit`

Extracts claims from documentation and verifies them against the codebase. Outputs a structured task list that agents can execute step-by-step.

**Claim types extracted:**
- **File paths**: Backtick paths like `src/core/mod.rs` - verified against filesystem
- **Directory paths**: Paths ending with `/` like `src/core/` - verified against filesystem
- **Code examples**: Fenced code blocks - flagged for manual verification

**Task statuses:**
- `verified`: Claim confirmed true, no action needed
- `broken`: Claim confirmed false, action required
- `needs_verification`: Cannot verify mechanically, agent must check

```sh
homeboy docs audit homeboy
homeboy docs audit data-machine
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
      "docs_scanned": 48,
      "claims_extracted": 81,
      "verified": 37,
      "broken": 44,
      "needs_verification": 0
    },
    "tasks": [
      {
        "doc": "architecture/output-system.md",
        "line": 12,
        "claim": "directory path `src/core/`",
        "type": "directory_path",
        "claim_value": "src/core/",
        "status": "verified"
      },
      {
        "doc": "developer-guide/architecture-overview.md",
        "line": 62,
        "claim": "file path `src/core/template.rs`",
        "type": "file_path",
        "claim_value": "src/core/template.rs",
        "status": "broken",
        "action": "File 'src/core/template.rs' not found. Search codebase for actual location or remove if deleted."
      }
    ],
    "changes_context": {
      "commits_since_tag": 5,
      "changed_files": ["src/core/mod.rs", "src/commands/docs.rs"],
      "priority_docs": ["architecture/output-system.md"]
    }
  }
}
```

**Agent workflow:**
1. Run `homeboy docs audit <component>`
2. For each task where `status != "verified"`:
   - If `broken`: Execute the action (fix or remove reference)
   - If `needs_verification`: Read the referenced file, verify claim, update if wrong
3. Re-run audit to confirm all tasks resolved

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

1. **Audit**: `homeboy docs audit <component>` - understand current state, find broken refs and gaps
2. **Learn**: `homeboy docs documentation/generation` - read guidelines
3. **Map**: `homeboy docs map <component>` - generate codebase map for AI context
4. **Generate**: `homeboy docs generate --json '<spec>'` - bulk create files
5. **Maintain**: `homeboy docs documentation/alignment` - keep docs current

## Errors

If a topic does not exist, the command fails with an error indicating the topic was not found.

If a component does not exist (for audit/map), the command fails with a component not found error.

## Related

- [Changelog command](changelog.md)
- [JSON output contract](../architecture/output-system.md)
