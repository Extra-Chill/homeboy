# `homeboy refactor`

## Synopsis

```sh
homeboy refactor <COMMAND>
```

## Description

Structural refactoring tools for renaming concepts across a codebase. Walks source files, finds all references to a term (with word-boundary matching and case-variant awareness), generates edits, and optionally applies them.

## Subcommands

### `rename`

```sh
homeboy refactor rename --from <FROM> --to <TO> [OPTIONS]
```

Rename a term across a codebase with automatic case-variant generation.

**Required:**
- `--from <FROM>`: Term to rename from
- `--to <TO>`: Term to rename to

**Target (one required):**
- `-c, --component <ID>`: Component ID (uses its `local_path` as the root)
- `--path <PATH>`: Directory path to refactor

**Options:**
- `--scope <SCOPE>`: What files to include — `code`, `config`, or `all` (default: `all`)
- `--literal`: Exact string matching — no boundary detection, no case variants
- `--write`: Apply changes to disk (default is dry-run)

## Standard Mode

By default, `refactor rename` generates case variants from the base term and matches them with word-boundary awareness.

Given `--from widget --to gadget`, the engine generates:

| Variant | From | To |
|---------|------|----|
| lowercase | `widget` | `gadget` |
| PascalCase | `Widget` | `Gadget` |
| UPPER_CASE | `WIDGET` | `GADGET` |
| plural | `widgets` | `gadgets` |
| plural PascalCase | `Widgets` | `Gadgets` |
| plural UPPER | `WIDGETS` | `GADGETS` |

**Boundary detection** ensures matches occur at sensible positions:
- Word boundaries: `pub mod widget;` ✓
- CamelCase joins: `WidgetManifest` ✓ (matches `Widget`)
- Snake_case compounds: `load_widget`, `WIDGET_DIR` ✓
- No false positives: `widgetry` ✗ (lowercase follows, not a boundary)

```sh
# Preview all changes (dry-run)
homeboy refactor rename --from widget --to gadget -c my-plugin

# Apply changes to disk
homeboy refactor rename --from widget --to gadget -c my-plugin --write

# Only rename in source code files (skip JSON/YAML/TOML)
homeboy refactor rename --from widget --to gadget --path ./src --scope code --write
```

## Literal Mode

With `--literal`, the engine matches the exact `--from` string as-is — no case variants, no boundary detection. Every substring occurrence is matched.

This is useful for compound renames where inserting characters breaks boundary rules:

```sh
# Rename a hyphenated slug
homeboy refactor rename --literal --from datamachine-events --to data-machine-events --path . --write

# Rename an underscored prefix
homeboy refactor rename --literal --from datamachine_events --to data_machine_events --path . --write

# Rename constants
homeboy refactor rename --literal --from DATAMACHINE_EVENTS --to DATA_MACHINE_EVENTS --path . --write
```

Since literal mode has no case-variant generation, run multiple passes for different casings (UPPER, snake, hyphen).

## File Walking

The engine walks the target directory recursively, scanning files with these extensions:

`rs`, `php`, `js`, `jsx`, `ts`, `tsx`, `mjs`, `json`, `toml`, `yaml`, `yml`, `md`, `txt`, `sh`, `bash`, `py`, `rb`, `go`, `swift`, `lock`

**Always skipped** (any depth): `node_modules`, `vendor`, `.git`, `.svn`, `.hg`

**Skipped at root only**: `build`, `dist`, `target`, `cache`, `tmp` — these are safe to skip at root (build artifacts), but scanned at deeper levels (e.g. a `scripts/build/` directory inside your project may contain source files).

## Collision Detection

Dry-run output includes warnings for potential issues:

- **File collisions**: A rename target path already exists on disk
- **Duplicate identifiers**: A rename would create two fields/variables with the same name at the same indentation level (e.g. renaming `widgets` → `gadgets` when a `gadgets` field already exists in the same struct)

Warnings are informational — `--write` applies changes even when warnings exist, but logs them to stderr.

## File Renames

In addition to content edits, the engine detects files and directories whose names contain the rename term and generates path renames. For example, in a project containing `src/widget/widget.rs`, renaming `widget` → `gadget` would generate the path rename `src/widget/widget.rs` → `src/gadget/gadget.rs`.

## JSON Output

```json
{
  "success": true,
  "data": {
    "command": "refactor.rename",
    "from": "widget",
    "to": "gadget",
    "scope": "all",
    "dry_run": true,
    "variants": [
      { "from": "widget", "to": "gadget", "label": "lowercase" },
      { "from": "Widget", "to": "Gadget", "label": "PascalCase" }
    ],
    "total_references": 42,
    "total_files": 8,
    "edits": [
      { "file": "src/core/widget.rs", "replacements": 12 }
    ],
    "file_renames": [
      { "from": "src/core/widget.rs", "to": "src/core/gadget.rs" }
    ],
    "warnings": [
      {
        "kind": "duplicate_identifier",
        "file": "src/core/config.rs",
        "line": 45,
        "message": "Duplicate identifier 'gadgets' at line 45 (first at line 30)"
      }
    ],
    "applied": false
  }
}
```

## Exit Code

- `0`: References found (and applied if `--write`)
- `1`: No references found

## Related

- [component](component.md)
- [docs audit](docs.md) — documentation-level auditing
- [JSON output contract](../architecture/output-system.md)
