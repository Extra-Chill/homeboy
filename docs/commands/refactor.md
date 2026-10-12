# `homeboy refactor`

## Synopsis

```sh
homeboy refactor rename --from <FROM> --to <TO> [OPTIONS]
```

## Description

`refactor` hosts one subcommand, `rename`: rename a term across a codebase with
automatic case-variant generation, word-boundary matching, and file renames.

Lint autofix lives in `homeboy review lint --fix`.

### `rename`

**Required:**
- `--from <FROM>`: Term to rename from
- `--to <TO>`: Term to rename to

**Target (defaults to the current directory):**
- `-c, --component <ID>`: Component ID, repeatable (uses its `local_path` as the root)
- `--components <ID,ID>`: Comma-separated component IDs
- `--path <PATH>`: Directory to refactor (single target only)

Multiple components produce a `refactor.bulk` result with one entry per target.

**Options:**
- `--scope <SCOPE>`: Which files to include: `code`, `config`, or `all` (default: `all`)
- `--literal`: Exact string matching with no boundary detection and no case variants
- `--files <GLOB>`: Include only files matching this glob (repeatable; any extension)
- `--exclude <GLOB>`: Exclude files matching this glob (repeatable)
- `--variant <FROM=TO>`: Add an explicit variant mapping (repeatable), for project-specific spellings such as acronyms
- `--no-file-renames`: Content edits only; leave file and directory names alone
- `--context <CONTEXT>`: Syntactic filter: `key` (strings and property access), `variable`/`var`, `parameter`/`param`, or `all` (default)
- `--write`: Apply changes to disk (default is a dry-run preview)

## Standard Mode

By default, `rename` splits `--from` and `--to` into words and generates every
naming convention for both, plus plural forms of each:

| Convention | From | To |
|------------|------|----|
| snake_case | `foo_bar` | `baz_qux` |
| UPPER_SNAKE | `FOO_BAR` | `BAZ_QUX` |
| PascalCase | `FooBar` | `BazQux` |
| camelCase | `fooBar` | `bazQux` |
| kebab | `foo-bar` | `baz-qux` |
| Title-Kebab-Case | `Foo-Bar` | `Baz-Qux` |
| Title_Snake_Case | `Foo_Bar` | `Baz_Qux` |
| Display Name | `Foo Bar` | `Baz Qux` |

When a generated variant has no matches, the engine searches case-insensitively
and reports the casing the codebase actually uses (for example `WPAgent` when
`WpAgent` was generated) as a discovered variant.

**Boundary detection** keeps matches at sensible positions:
- Word boundaries: `pub mod widget;` matches
- CamelCase joins: `WidgetManifest` matches `Widget`
- Snake_case compounds: `load_widget`, `WIDGET_DIR` match
- No false positives: `widgetry` does not match `widget`

```sh
# Preview all changes (dry-run)
homeboy refactor rename --from widget --to gadget -c my-plugin

# Apply changes to disk
homeboy refactor rename --from widget --to gadget -c my-plugin --write

# Only rename in source code files (skip JSON/YAML/TOML)
homeboy refactor rename --from widget --to gadget --path ./src --scope code --write
```

## Literal Mode

With `--literal`, the engine matches the exact `--from` string with no case
variants and no boundary detection; every substring occurrence matches. Use it
for compound renames where inserting characters breaks boundary rules:

```sh
homeboy refactor rename --literal --from sampleplugin-events --to sample-plugin-events --path . --write
homeboy refactor rename --literal --from SAMPLEPLUGIN_EVENTS --to SAMPLE_PLUGIN_EVENTS --path . --write
```

Run one pass per casing in literal mode.

## File Walking

The engine walks the target directory recursively. `--scope code` scans source
files except JSON/TOML/YAML; `--scope config` scans only JSON/TOML/YAML; `all`
scans the default source extensions plus repository config dotfiles such as
`.editorconfig` and `.npmrc`.

**Always skipped** (any depth): `node_modules`, `vendor`, `.git`, `.svn`, `.hg`

**Skipped at root only**: build output directories such as `build`, `dist`,
`target`, `cache`, and `tmp`. Directories with these names deeper in the tree
are scanned.

## File Renames

Files whose paths contain a variant are renamed too. In a project containing
`src/widget/widget.rs`, renaming `widget` to `gadget` produces
`src/widget/widget.rs` → `src/gadget/gadget.rs`, including non-source files
inside renamed directories. Source directories emptied by the move are removed.

## Collision Detection

Dry-run output includes warnings for:

- **File collisions**: a rename target path already exists on disk
- **Duplicate identifiers**: a rename would create two fields or variables with the same name at the same indentation level

Warnings are informational. `--write` applies changes when warnings exist and logs them to stderr.

## Write Safety

`--write` is all-or-nothing. Content edits and file moves are journaled as they
land; if any step fails (for example a destination that appeared after
planning), every applied edit and move is reverted and the command fails with
the cause and the rollback outcome.

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
      { "from": "widget", "to": "gadget", "label": "exact" },
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

- `0`: The preview or apply completed, including when no references were found
- `1`: Invalid arguments, unresolved targets, a failed apply, or any failed target in a multi-component run

## Related

- [component](component.md)
- [review](review.md)
- [JSON output contract](../architecture/output-system.md)
