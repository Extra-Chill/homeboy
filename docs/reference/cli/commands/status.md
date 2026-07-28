<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy status` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/status.md](../../../commands/status.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy status`

```sh
homeboy status [OPTIONS] [PROJECT]
```

Actionable component status overview

| Argument | Required | Description |
| --- | --- | --- |
| `[PROJECT]` | no | Project ID — show version dashboard for a project's components |

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<ID>` | Scope: project id |
| `--fleet` | `<ID>` | Scope: fleet id |
| `--component` | `<ID>` | Scope: registered component id |
| `--rig` | `<ID>` | Scope: local rig id |
| `--path` | `<PATH>` | Scope: checkout path, bypassing the registry |
| `--workspace` | flag | Scope: every configured workspace repo |
| `--full` | flag | Show the full workspace/context report (the old init behavior) |
| `--uncommitted` | flag | Show only components with uncommitted changes |
| `--needs-release` | flag | Show only components that need a release |
| `--ready` | flag | Show only components ready to deploy |
| `--docs-only` | flag | Show only components with docs-only changes |
| `-a`, `--all` | flag | Show all components regardless of current directory context |
| `--outdated` | flag | Show only outdated components (local != remote) |
| `--timings` | flag | Emit status phase progress to stderr and include phase timings in JSON |
| `--refresh` | flag | Refresh remote Git refs before calculating drift and release state |
| `--unreleased` | flag | Show only components carrying merged-but-unreleased work (commits on origin/<default-branch> that are past the latest release tag) |

