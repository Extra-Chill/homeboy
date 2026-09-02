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
| `--placement` | `<PLACEMENT>` | Select where eligible work executes. `auto` (default) follows command policy; `lab` selects an eligible ready runner; `local` is an explicit authorized override. Use `--runner <id>` instead to pin one runner Values: `auto`, `local`, `lab`, `lab-or-local`. |
| `--detach-after-handoff` | flag | Submit to Lab and return after durable controller handoff. Omit it to keep observing the remote lifecycle, which remains the default |
| `--artifact-root` | `<DIR>` | Directory where persisted run artifacts are copied. Overrides HOMEBOY_ARTIFACT_ROOT and global config /artifact_root |
| `--runner` | `<RUNNER_ID>` | Pin portable work to a connected Lab runner. This implies Lab placement; use `--placement <policy>` instead to select placement without pinning |
| `--allow-dirty-lab-workspace` | flag | Permit Lab git workspace materialization to overwrite a dirty runner-side checkout |
| `--skip-deps-hydration` | flag | Skip post-materialization dependency hydration for Lab offloads. When set, Homeboy does not run the detected provider install (e.g. `composer install`, `npm ci`) in the materialized runner workspace before the command starts |
| `--preserve-workspace-on-failure` | flag | Preserve a failed Lab workspace for bounded TTL-based inspection |
| `--runner-env` | `<KEY=VALUE>` | Add a job-scoped environment variable to a Lab offload without mutating runner config |
| `--runner-secret-env` | `<NAME>` | Reference a runner-owned secret environment variable for a Lab offload. The runner resolves this identity; Homeboy never accepts its value here |
| `--lab-env-json` | `<JSON>` | Add job-scoped Lab offload environment from a JSON object without mutating runner config |
| `--runner-workspace-root` | `<DIR>` | Override the selected runner workspace root for this Lab offload only |

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
