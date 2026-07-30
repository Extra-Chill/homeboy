<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy stack` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/stack.md](../../../commands/stack.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy stack`

```sh
homeboy stack <COMMAND>
```

Manage stacks (combined-fixes branches built from base + cherry-picked PRs)

| Subcommand | Summary |
| --- | --- |
| `homeboy stack list` | List all installed stack specs |
| `homeboy stack show` | Show a stack spec |
| `homeboy stack create` | Create a new stack spec |
| `homeboy stack add-pr` | Append a PR entry to the stack's `prs` array |
| `homeboy stack remove-pr` | Remove a PR entry from the stack's `prs` array |
| `homeboy stack apply` | Materialize a stack: cherry-pick `base + prs` onto `target` |
| `homeboy stack rebase` | Rebuild the target branch from fresh base + current spec PRs |
| `homeboy stack status` | Read-only status report — upstream PR state + local target state |
| `homeboy stack sync` | Rebase the target branch onto fresh base AND auto-drop merged PRs from the spec |
| `homeboy stack push` | Push the materialized target branch to its configured remote |
| `homeboy stack diff` | Preview what `stack sync` would change without mutating target or spec |
| `homeboy stack inspect` | Spec-less inspection of the current branch as a stack of commits. Replaces the previous `homeboy git stack` command (re-homed into the stack domain) |

## `homeboy stack list`

```sh
homeboy stack list
```

List all installed stack specs

## `homeboy stack show`

```sh
homeboy stack show <STACK_ID>
```

Show a stack spec

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

## `homeboy stack create`

```sh
homeboy stack create [OPTIONS] <STACK_ID>
```

Create a new stack spec

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID (used as filename: `~/.config/homeboy/stacks/<id>.json`) |

| Option | Value | Description |
| --- | --- | --- |
| `--component` | `<COMPONENT>` | Component identifier (informational; future: rig binding key) |
| `--component-path` | `<COMPONENT_PATH>` | Local checkout path. Supports `~` and `${env.VAR}` expansion |
| `--base` | `<BASE>` | Upstream ref to rebuild from, as `<remote>/<branch>` (e.g. `origin/trunk`) |
| `--target` | `<TARGET>` | Target combined-fixes branch as `<remote>/<branch>` (e.g. `fork/dev/combined-fixes`) |
| `--description` | `<DESCRIPTION>` | Optional human-readable description |

## `homeboy stack add-pr`

```sh
homeboy stack add-pr [OPTIONS] <STACK_ID> <REPO> <NUMBER>
```

Append a PR entry to the stack's `prs` array

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |
| `<REPO>` | yes | `<owner>/<repo>` coordinate (e.g. `example-org/studio`) |
| `<NUMBER>` | yes | PR number |

| Option | Value | Description |
| --- | --- | --- |
| `--note` | `<NOTE>` | Optional human-readable note |

## `homeboy stack remove-pr`

```sh
homeboy stack remove-pr [OPTIONS] <STACK_ID> <NUMBER>
```

Remove a PR entry from the stack's `prs` array

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |
| `<NUMBER>` | yes | PR number to remove. If multiple entries match (different repos), pass `--repo` to disambiguate |

| Option | Value | Description |
| --- | --- | --- |
| `--repo` | `<REPO>` | Restrict removal to this `<owner>/<repo>` (when the same PR number appears in multiple repos in the stack) |

## `homeboy stack apply`

```sh
homeboy stack apply [OPTIONS] <STACK_ID>
```

Materialize a stack: cherry-pick `base + prs` onto `target`.

Stops on the first cherry-pick conflict, leaves the conflicted pick in the checkout, and prints the git commands that resolve or abandon it.

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

| Option | Value | Description |
| --- | --- | --- |
| `--abort-on-conflict` | flag | Run `git cherry-pick --abort` on conflict instead of leaving the conflicted pick in place for manual resolution |

## `homeboy stack rebase`

```sh
homeboy stack rebase [OPTIONS] <STACK_ID>
```

Rebuild the target branch from fresh base + current spec PRs.

Unlike `sync`, `rebase` never edits the stack spec: merged PRs stay listed until an explicit `sync` or `remove-pr`.

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

| Option | Value | Description |
| --- | --- | --- |
| `--abort-on-conflict` | flag | Run `git cherry-pick --abort` on conflict instead of leaving the conflicted pick in place for manual resolution |

## `homeboy stack status`

```sh
homeboy stack status <STACK_ID>
```

Read-only status report — upstream PR state + local target state

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

## `homeboy stack sync`

```sh
homeboy stack sync [OPTIONS] <STACK_ID>
```

Rebase the target branch onto fresh base AND auto-drop merged PRs from the spec.

`sync` is the holistic upkeep verb for a combined-fixes branch: PRs that have been merged upstream (and whose content is in base) are removed from the spec; everything else is cherry-picked onto a freshly-rebuilt target. On the first cherry-pick conflict, the in-progress pick is left in the checkout and a resolve message printed.

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Print what WOULD drop and pick without mutating spec or target branch |
| `--abort-on-conflict` | flag | Run `git cherry-pick --abort` on conflict instead of leaving the conflicted pick in place for manual resolution |

## `homeboy stack push`

```sh
homeboy stack push <STACK_ID>
```

Push the materialized target branch to its configured remote

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

## `homeboy stack diff`

```sh
homeboy stack diff <STACK_ID>
```

Preview what `stack sync` would change without mutating target or spec

| Argument | Required | Description |
| --- | --- | --- |
| `<STACK_ID>` | yes | Stack ID |

## `homeboy stack inspect`

```sh
homeboy stack inspect [OPTIONS] [COMPONENT_ID]
```

Spec-less inspection of the current branch as a stack of commits. Replaces the previous `homeboy git stack` command (re-homed into the stack domain)

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT_ID]` | no | Component ID. When omitted, auto-detected from CWD |

| Option | Value | Description |
| --- | --- | --- |
| `--base` | `<REF>` | Base ref to compare against. Defaults to `@{upstream}` of the current branch |
| `--no-pr` | flag | Skip the GitHub PR lookup pass |
| `--repo` | `<OWNER/NAME>` | Scope PR lookups to a specific GitHub repo (`owner/name`) |
| `--path` | `<PATH>` | Workspace path to operate on directly |
