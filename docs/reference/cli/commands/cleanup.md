<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy cleanup` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/cleanup.md](../../../commands/cleanup.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy cleanup`

```sh
homeboy cleanup [OPTIONS] [COMMAND]
```

Remove declared reconstructable artifacts from managed worktrees

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply cleanup across the selected categories. Omit for inventory dry-run output |
| `--include` | `<INCLUDE>` | Include only these cleanup categories. Comma-separated or repeatable. `runner-downloads` is opt-in only: it holds artifacts an operator asked Homeboy to fetch, so a bare sweep never includes it Values: `repo-artifacts`, `task-worktrees`, `worktree-providers`, `terminal-runs`, `persisted-run-artifacts`, `orphaned-artifact-bytes`, `runner-downloads`, `runner-binary-caches`, `remote-lab-workspaces`, `runtime-tmp`, `controller-scratch`, `shared-cargo-targets`, `controller-runtimes`. |
| `--exclude` | `<EXCLUDE>` | Exclude these cleanup categories. Comma-separated or repeatable Values: `repo-artifacts`, `task-worktrees`, `worktree-providers`, `terminal-runs`, `persisted-run-artifacts`, `orphaned-artifact-bytes`, `runner-downloads`, `runner-binary-caches`, `remote-lab-workspaces`, `runtime-tmp`, `controller-scratch`, `shared-cargo-targets`, `controller-runtimes`. |
| `--older-than-days` | `<DAYS>` | Override the configured terminal-run retention window for this invocation |
| `--runtime-tmp-managed-older-than-days` | `<DAYS>` | Override the age floor for metadata-backed runtime temp entries only. Unmanaged entries retain the configured runtime temp age floor |
| `--limit` | `<N>` | Override the configured maximum number of persisted artifacts inspected |
| `--full` | flag | Include every controller-scratch candidate and retained-resource detail. Default output keeps representative detail within the shared response budget |
| `--cursor` | `<CURSOR>` | Continue a bounded shared-store cleanup inventory from this cursor |

| Subcommand | Summary |
| --- | --- |
| `homeboy cleanup artifacts` | Inspect or remove declared reconstructable artifacts across repo worktrees |
| `homeboy cleanup worktrees` | Aggregate cleanup across configured external worktree providers |
| `homeboy cleanup retained-storage` | Explain retained Homeboy storage without deleting or reconciling resources |
| `homeboy cleanup automatic-retention` | Run one configured, bounded retention pass |

## `homeboy cleanup artifacts`

```sh
homeboy cleanup artifacts [OPTIONS]
```

Inspect or remove declared reconstructable artifacts across repo worktrees

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Apply cleanup. Omit for dry-run output |
| `--self` | flag | Clean artifacts from the Homeboy source checkout that built this binary |
| `--path` | `<PATH>` | Resolve managed worktrees from this checkout instead of the current directory |
| `--temp-root` | `<PATH>` | Also scan this temp root for detached Homeboy build artifacts. Repeatable |
| `--sort` | `<SORT>` | Sort artifact candidates before reporting or applying cleanup Values: `discovery`, `size`. |
| `--limit` | `<N>` | Limit artifact candidates reported or removed after sorting |
| `--merged-only` | flag | Only reclaim artifacts from worktrees whose branch is already merged into its upstream. Preserves in-progress cooks' build dirs |
| `--min-age-days` | `<DAYS>` | Only reclaim artifacts untouched for at least this many days. Composes with any age floor a declaration owner sets; the stricter one wins |
| `--include-active-worktrees` | flag | Also reclaim extension-declared artifacts from checkouts registered as active task worktrees. Those are protected by default because removing an install tree leaves a live checkout unusable until it is rehydrated |

## `homeboy cleanup worktrees`

```sh
homeboy cleanup worktrees [OPTIONS]
```

Aggregate cleanup across configured external worktree providers

| Option | Value | Description |
| --- | --- | --- |
| `--provider` | `<ID>` | Cleanup a specific configured provider. Repeatable |
| `--all-providers` | flag | Cleanup every enabled configured provider |
| `--apply` | flag | Apply cleanup. Omit for provider preview/dry-run output |

## `homeboy cleanup retained-storage`

```sh
homeboy cleanup retained-storage [OPTIONS]
```

Explain retained Homeboy storage without deleting or reconciling resources.

Reports lifecycle aggregates alongside root filesystem accounting, top-level stores, largest child paths, ownership classification, and cleanup guidance.

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<LIMIT>` | Maximum largest-byte examples to return. The report always aggregates all inspected sources |
| `--cursor` | `<CURSOR>` | Continue largest-byte examples after this deterministic reference token |

## `homeboy cleanup automatic-retention`

```sh
homeboy cleanup automatic-retention
```

Run one configured, bounded retention pass
