<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy triage` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/triage.md](../../../commands/triage.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy triage`

```sh
homeboy triage [COMMAND]
```

Attention reports and watch utilities for components, projects, fleets, and rigs

| Subcommand | Summary |
| --- | --- |
| `homeboy triage component` | Triage one registered component |
| `homeboy triage project` | Triage every component attached to a project |
| `homeboy triage fleet` | Triage unique components used across a fleet |
| `homeboy triage rig` | Triage components declared in a local rig spec |
| `homeboy triage workspace` | Triage every configured project, rig, and registered component once per repo |
| `homeboy triage landing` | Summarize mergeability and check blockers for a PR landing fleet |

## `homeboy triage component`

```sh
homeboy triage component [OPTIONS] [COMPONENT_ID]
```

Triage one registered component.

When `--path <CHECKOUT>` is supplied, the registry is bypassed and the GitHub remote is resolved directly from the checkout's `origin`. Useful for unregistered checkouts (CI runners, ad-hoc clones, worktrees) or when a component's registry record is broken.

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID. Optional when `--path` is supplied |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<CHECKOUT>` | Workspace path to triage directly, bypassing the registry |

## `homeboy triage project`

```sh
homeboy triage project <PROJECT_ID>
```

Triage every component attached to a project

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | _no help text_ |

## `homeboy triage fleet`

```sh
homeboy triage fleet <FLEET_ID>
```

Triage unique components used across a fleet

| Argument | Required | Description |
| --- | --- | --- |
| `<FLEET_ID>` | yes | _no help text_ |

## `homeboy triage rig`

```sh
homeboy triage rig <RIG_ID>
```

Triage components declared in a local rig spec

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | _no help text_ |

## `homeboy triage workspace`

```sh
homeboy triage workspace
```

Triage every configured project, rig, and registered component once per repo

## `homeboy triage landing`

```sh
homeboy triage landing [OPTIONS] [PR_REFS]...
```

Summarize mergeability and check blockers for a PR landing fleet

| Argument | Required | Description |
| --- | --- | --- |
| `[PR_REFS]...` | no | PR numbers, owner/repo#number refs, or GitHub PR URLs |

| Option | Value | Description |
| --- | --- | --- |
| `--repo` | `<REPO>` | Resolve bare PR numbers against this GitHub repo (`owner/name` or URL) |
| `--branch` | `<PATTERN>` | Include open PRs whose source branch matches this pattern. Repeatable |
| `--source-issue` | `<NUMBER>` | Include PRs linked to this issue number in each resolved repo. Repeatable |
| `--ordered` | flag | Preserve supplied PR order and emit dependent-branch rebase plans |
| `--project` | `<ID>` | Scope: project id |
| `--fleet` | `<ID>` | Scope: fleet id |
| `--component` | `<ID>` | Scope: registered component id |
| `--rig` | `<ID>` | Scope: local rig id |
| `--path` | `<PATH>` | Scope: checkout path, bypassing the registry |
| `--workspace` | flag | Scope: every configured workspace repo |

