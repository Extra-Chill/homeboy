<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy refactor` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/refactor.md](../../../commands/refactor.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy refactor`

```sh
homeboy refactor [OPTIONS] [COMPONENT] [COMMAND]
```

Structural refactoring (rename terms across codebase)

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--from` | `<SOURCE>` | Include a specific proposal source (repeatable): audit, lint, test, all |
| `--all` | flag | Compatibility alias for `--from all` |
| `--changed-since` | `<CHANGED_SINCE>` | Only include files changed since a git ref (branch, tag, or SHA) |
| `--only` | `<kind>` | Restrict audit-generated fixes to these fix kinds (repeatable) |
| `--exclude` | `<kind>` | Exclude audit-generated fixes for these fix kinds (repeatable) |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--force` | flag | Skip the clean working tree check (for CI or when you know what you're doing) |
| `--write` | flag | _no help text_ |
| `--commit` | flag | After applying fixes, stage all changes and commit. Only effective with --write. The commit message is built from fix results |
| `--git-identity` | `<GIT_IDENTITY>` | Git identity for the commit (used with --commit). Use "bot" for the default CI bot identity, or "Name <email>" for custom |

| Subcommand | Summary |
| --- | --- |
| `homeboy refactor rename` | Rename a term across the codebase with case-variant awareness |
| `homeboy refactor add` | Add imports, stubs, or fixes to source files |
| `homeboy refactor move` | Move items or entire files between modules |
| `homeboy refactor propagate` | Add missing fields to struct instantiations after a struct definition changes |
| `homeboy refactor collapse-defaults` | Collapse default-valued fields in struct instantiations into `..Default::default()` (the inverse of `propagate`) |
| `homeboy refactor transform` | Apply an ad-hoc pattern-based find/replace transform across a codebase |
| `homeboy refactor decompose` | Decompose a large source file into a directory of smaller modules |
| `homeboy refactor refs` | Read-only reference discovery for a symbol or term |
| `homeboy refactor undo` | Undo the last write operation snapshot |

## `homeboy refactor rename`

```sh
homeboy refactor rename [OPTIONS]
```

Rename a term across the codebase with case-variant awareness

| Option | Value | Description |
| --- | --- | --- |
| `--from` | `<FROM>` | Term to rename from |
| `--to` | `<TO>` | Term to rename to |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--scope` | `<SCOPE>` | Scope: code, config, all (default: all) |
| `--literal` | flag | Exact string matching (no boundary detection, no case variants) |
| `--files` | `<GLOB>` | Include only files matching this glob (repeatable) |
| `--exclude` | `<GLOB>` | Exclude files matching this glob (repeatable) |
| `--variant` | `<FROM=TO>` | Add an explicit variant mapping as FROM=TO (repeatable) |
| `--no-file-renames` | flag | Disable file/directory path renames (content edits only) |
| `--context` | `<CONTEXT>` | Syntactic context filter: key (strings/property access), variable/var, parameter/param, all (default — match everything) |
| `--write` | flag | _no help text_ |

## `homeboy refactor add`

```sh
homeboy refactor add [OPTIONS]
```

Add imports, stubs, or fixes to source files

Two modes: From audit: `refactor add --from-audit @audit.json [--write]` Explicit: `refactor add --import "use serde::Serialize;" --to "src/**/*.rs" [--write]`

| Option | Value | Description |
| --- | --- | --- |
| `--from-audit` | `<AUDIT_JSON>` | Apply fixes from saved audit JSON (supports @file, -, or inline JSON) |
| `--import` | `<IMPORT>` | Import/use statement to add (explicit mode) |
| `--to` | `<PATTERN>` | Target file or glob pattern for explicit additions |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--write` | flag | _no help text_ |

## `homeboy refactor move`

```sh
homeboy refactor move [OPTIONS]
```

Move items or entire files between modules

Item mode: `refactor move --item has_import --from src/conventions.rs --to src/import_matching.rs` File mode: `refactor move --file src/core/hooks.rs --to src/core/engine/hooks.rs`

| Option | Value | Description |
| --- | --- | --- |
| `--item` | `<NAME>` | Name(s) of items to move (functions, structs, enums, consts). When omitted with --file, moves the entire file |
| `--file` | `<FILE>` | Move an entire module file to a new location. Rewrites all imports and updates mod.rs declarations |
| `--from` | `<FILE>` | Source file (for item mode — relative to component/path root) |
| `--to` | `<FILE>` | Destination file (relative to component/path root, created if needed) |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--write` | flag | _no help text_ |

## `homeboy refactor propagate`

```sh
homeboy refactor propagate [OPTIONS]
```

Add missing fields to struct instantiations after a struct definition changes

Scans the codebase for instantiations of the named struct, detects which fields are missing, and inserts them with sensible defaults (None, vec![], false, etc.).

Example: `refactor propagate --struct-name FileFingerprint --component homeboy`

| Option | Value | Description |
| --- | --- | --- |
| `--struct-name` | `<NAME>` | Name of the struct to propagate fields for |
| `--definition` | `<FILE>` | File containing the struct definition (auto-detected if omitted) |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--write` | flag | _no help text_ |

## `homeboy refactor collapse-defaults`

```sh
homeboy refactor collapse-defaults [OPTIONS]
```

Collapse default-valued fields in struct instantiations into `..Default::default()` (the inverse of `propagate`).

Scans the codebase for instantiations of the named struct and, for each, removes fields whose value equals the type's default (None, Vec::new(), Value::Null, String::new(), false, 0, etc.), replacing them with a single trailing `..Default::default()`. Conservative: skips literals that already spread, contain an interspersed comment, or set an unknown-type field. The struct must have a `Default` impl for the result to compile.

Dry-run by default — pass `--write` to apply.

Example: `refactor collapse-defaults --struct-name AgentTaskOutcome --component homeboy`

| Option | Value | Description |
| --- | --- | --- |
| `--struct-name` | `<NAME>` | Name of the struct to collapse defaults for |
| `--definition` | `<FILE>` | File containing the struct definition (auto-detected if omitted) |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--write` | flag | _no help text_ |

## `homeboy refactor transform`

```sh
homeboy refactor transform [OPTIONS]
```

Apply an ad-hoc pattern-based find/replace transform across a codebase

Example: `refactor transform --find "old" --replace "new" --files "**/*.php" --component C`

Replacement templates support capture group refs ($1, $2, ${name}), case transforms ($1:lower, $1:upper, $1:kebab, $1:snake, $1:pascal, $1:camel), and literal $ via $$ (important for PHP code where every variable starts with $).

Backslash escapes collapse before regex replacement: `\\` → one literal backslash, `\n` → newline, `\t` → tab, `\r` → CR, `\"` / `\'` → the quote. Write `\\WP_Foo` to emit `\WP_Foo` on disk (useful for PHP fully-qualified class names). Unknown `\X` sequences pass through as-is.

| Option | Value | Description |
| --- | --- | --- |
| `--find` | `<REGEX>` | Regex pattern to find |
| `--replace` | `<TEMPLATE>` | Replacement template. Supports $1, $2 capture group refs, ${name} named groups, $1:lower/:upper/:kebab/:snake/:pascal/:camel case transforms, and $$ for a literal dollar sign. Backslash escapes are collapsed: \\ → one literal backslash, \n/\t/\r/\0 → the control character, \" / \' → the quote |
| `--files` | `<GLOB>` | Glob pattern for files to apply to (default: **/*) |
| `--context` | `<CONTEXT>` | Match context: "line" (default, per-line matching) or "file" (whole-file, enables multi-line regex with (?s) dotall flag for patterns spanning newlines) |
| `--full-match-details` | flag | Include every match detail in JSON output instead of the default bounded sample |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--write` | flag | _no help text_ |

## `homeboy refactor decompose`

```sh
homeboy refactor decompose [OPTIONS]
```

Decompose a large source file into a directory of smaller modules

| Option | Value | Description |
| --- | --- | --- |
| `--file` | `<FILE>` | Source file to decompose (relative to component/path root) |
| `--strategy` | `<STRATEGY>` | Planning strategy (currently: grouped) |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--write` | flag | _no help text_ |

## `homeboy refactor refs`

```sh
homeboy refactor refs [OPTIONS] <SYMBOL>
```

Read-only reference discovery for a symbol or term

| Argument | Required | Description |
| --- | --- | --- |
| `<SYMBOL>` | yes | Symbol or term to find |

| Option | Value | Description |
| --- | --- | --- |
| `-c`, `--component` | `<ID>` | Target a component by ID (repeatable) |
| `--components` | `<ID[,ID...]>` | Target multiple components with a comma-separated list |
| `--path` | `<PATH>` | Override the source root for a single target |
| `--scope` | `<SCOPE>` | Scope: code, config, all |
| `--literal` | flag | Exact string matching (no boundary detection, no case variants) |
| `--files` | `<GLOB>` | Include only files matching this glob (repeatable) |
| `--exclude` | `<GLOB>` | Exclude files matching this glob (repeatable) |
| `--context` | `<CONTEXT>` | Syntactic context filter: key, variable/var, parameter/param, all |

## `homeboy refactor undo`

```sh
homeboy refactor undo [OPTIONS] [COMMAND]
```

Undo the last write operation snapshot

| Option | Value | Description |
| --- | --- | --- |
| `--id` | `<ID>` | Restore a specific snapshot by ID (default: latest) |

| Subcommand | Summary |
| --- | --- |
| `homeboy refactor undo list` | List available undo snapshots |
| `homeboy refactor undo delete` | Delete a snapshot without restoring |

## `homeboy refactor undo list`

```sh
homeboy refactor undo list
```

List available undo snapshots

## `homeboy refactor undo delete`

```sh
homeboy refactor undo delete <ID>
```

Delete a snapshot without restoring

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Snapshot ID to delete |
