<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy runtime` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/runtime.md](../../../commands/runtime.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy runtime`

```sh
homeboy runtime <COMMAND>
```

Inspect core-owned runtime helper assets

| Subcommand | Summary |
| --- | --- |
| `homeboy runtime helper` | Inspect core-bundled runtime helper paths exposed to extension runners |
| `homeboy runtime refresh` | Refresh a shared runtime package from a source repository or directory |
| `homeboy runtime promotion-takeover` | Explicitly archive a proven dead or expired runtime-promotion lease |
| `homeboy runtime controller-prune` | Plan or apply pruning for unreferenced immutable controller runtimes |
| `homeboy runtime materialize-controller` | Build and pin an exact controller candidate, optionally continuing one command |

## `homeboy runtime helper`

```sh
homeboy runtime helper <COMMAND>
```

Inspect core-bundled runtime helper paths exposed to extension runners

| Subcommand | Summary |
| --- | --- |
| `homeboy runtime helper path` | Print the materialized path for a known core runtime helper |

## `homeboy runtime helper path`

```sh
homeboy runtime helper path [OPTIONS] <HELPER>
```

Print the materialized path for a known core runtime helper

| Argument | Required | Description |
| --- | --- | --- |
| `<HELPER>` | yes | Known helper filename or injected HOMEBOY_RUNTIME_* env var name |

| Option | Value | Description |
| --- | --- | --- |
| `--plain` | flag | Print only the path, for shell bootstrap usage |

## `homeboy runtime refresh`

```sh
homeboy runtime refresh [OPTIONS] <RUNTIME_ID>
```

Refresh a shared runtime package from a source repository or directory

| Argument | Required | Description |
| --- | --- | --- |
| `<RUNTIME_ID>` | yes | Runtime package ID to materialize |

| Option | Value | Description |
| --- | --- | --- |
| `--source` | `<SOURCE>` | Git URL, repo root, or runtime package directory to install from |
| `--ref` | `<REVISION>` | Git ref to check out for URL sources (branch, tag, or commit) |

## `homeboy runtime promotion-takeover`

```sh
homeboy runtime promotion-takeover
```

Explicitly archive a proven dead or expired runtime-promotion lease

## `homeboy runtime controller-prune`

```sh
homeboy runtime controller-prune [OPTIONS]
```

Plan or apply pruning for unreferenced immutable controller runtimes

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Execute the mutation. Without this flag the command reports a plan only |
| `--dry-run` | flag | Explicitly request the plan-only default. Never mutates |
| `--ignore-retention` | flag | Purge every unreferenced pin, ignoring the configured controller runtime retention window. Destructive: prefer the configured window |

## `homeboy runtime materialize-controller`

```sh
homeboy runtime materialize-controller [OPTIONS] [INVOCATION]...
```

Build and pin an exact controller candidate, optionally continuing one command

| Argument | Required | Description |
| --- | --- | --- |
| `[INVOCATION]...` | no | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--source` | `<SOURCE>` | _no help text_ |
| `--commit` | `<COMMIT>` | _no help text_ |
| `--identity` | `<IDENTITY>` | _no help text_ |
