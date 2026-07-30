<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy self` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/self.md](../../../commands/self.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy self`

```sh
homeboy self <COMMAND>
```

Inspect the active Homeboy binary and install signals

| Subcommand | Summary |
| --- | --- |
| `homeboy self status` | Report active binary, version, and nearby install/update signals |
| `homeboy self identity` | Report the active binary build identity without external probes |
| `homeboy self doctor` | Report one authoritative binary/runtime view across the controller and every configured runner, including version drift signals and host resource pressure (machine load, hot Homeboy-adjacent processes, rig leases) |
| `homeboy self cleanup-runtime-tmp` | Plan or delete orphaned Homeboy runtime temp entries |
| `homeboy self docs` | Display CLI documentation |

## `homeboy self status`

```sh
homeboy self status
```

Report active binary, version, and nearby install/update signals

## `homeboy self identity`

```sh
homeboy self identity
```

Report the active binary build identity without external probes

## `homeboy self doctor`

```sh
homeboy self doctor
```

Report one authoritative binary/runtime view across the controller and every configured runner, including version drift signals and host resource pressure (machine load, hot Homeboy-adjacent processes, rig leases)

## `homeboy self cleanup-runtime-tmp`

```sh
homeboy self cleanup-runtime-tmp [OPTIONS]
```

Plan or delete orphaned Homeboy runtime temp entries

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Delete planned temp entries. Without this flag, only reports the plan |
| `--older-than-days` | `<OLDER_THAN_DAYS>` | Only include entries older than this many days. Defaults to the configured `retention.runtime_tmp_days` |
| `--prefix` | `<PREFIX>` | Only include entries whose directory/file name starts with this prefix |
| `--limit` | `<LIMIT>` | Maximum temp entries to inspect in one invocation. Defaults to the configured `retention.limit` |
| `--run-max-bytes` | `<RUN_MAX_BYTES>` | Maximum aggregate bytes retained for failed runtime run evidence. Defaults to the configured `retention.runtime_run_max_bytes` |
| `--run-max-count` | `<RUN_MAX_COUNT>` | Maximum failed runtime run directories retained. Defaults to the configured `retention.runtime_run_max_count` |
| `--cursor` | `<CURSOR>` | Continue bounded runtime-run inspection from a prior next_cursor |

## `homeboy self docs`

```sh
homeboy self docs [TOPIC] [COMMAND]
```

Display CLI documentation

| Argument | Required | Description |
| --- | --- | --- |
| `[TOPIC]` | no | Topic path (e.g., 'commands/deploy') or 'list' to show available topics |

| Subcommand | Summary |
| --- | --- |
| `homeboy self docs map` | Generate a machine-optimized codebase map for AI documentation |

## `homeboy self docs map`

```sh
homeboy self docs map [OPTIONS] <COMPONENT_ID>
```

Generate a machine-optimized codebase map for AI documentation

| Argument | Required | Description |
| --- | --- | --- |
| `<COMPONENT_ID>` | yes | Component to analyze |

| Option | Value | Description |
| --- | --- | --- |
| `--source-dirs` | `<SOURCE_DIRS>` | Source directories to analyze (comma-separated). Overrides auto-detection |
| `--include-private` | flag | Include private methods and internals (default: public API surface only) |
| `--write` | flag | Write markdown documentation files to disk (default: JSON to stdout) |
| `--output-dir` | `<OUTPUT_DIR>` | Output directory for markdown files (default: docs) |
