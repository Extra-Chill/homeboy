<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# Homeboy CLI reference (generated)

`homeboy` exposes 568 visible commands across 40 top-level command families. Every page below is generated from the clap command tree in `crates/homeboy-cli`, so it cannot drift from the binary.

Hand-written narrative lives in the [commands index](../../../commands/commands-index.md). Global flags are documented in [the root command reference](../homeboy-root-command.md). Machine-readable safety, docs, output, and Lab metadata come from `homeboy contract manifest`.

| Command | Reference | Summary |
| --- | --- | --- |
| `homeboy activity` | [activity.md](activity.md) | Unified view of active and recently finished Homeboy work |
| `homeboy agent-task` | [agent-task.md](agent-task.md) | Run generic agent task plans |
| `homeboy deferred-workload` | [deferred-workload.md](deferred-workload.md) | Resume portable workloads deferred until a runner is ready |
| `homeboy project` | [project.md](project.md) | Manage project configuration |
| `homeboy ssh` | [ssh.md](ssh.md) | SSH into a project server or configured server |
| `homeboy server` | [server.md](server.md) | Manage SSH server configurations |
| `homeboy bench` | [bench.md](bench.md) | Run performance benchmarks for a component |
| `homeboy fuzz` | [fuzz.md](fuzz.md) | Run generic fuzz workloads for a component |
| `homeboy trace` | [trace.md](trace.md) | Capture black-box behavioral traces for a component |
| `homeboy db` | [db.md](db.md) | Database operations |
| `homeboy deps` | [deps.md](deps.md) | Manage component dependencies |
| `homeboy file` | [file.md](file.md) | Remote file operations |
| `homeboy fleet` | [fleet.md](fleet.md) | Manage fleets (groups of projects) |
| `homeboy logs` | [logs.md](logs.md) | Remote log viewing |
| `homeboy triage` | [triage.md](triage.md) | Attention reports and watch utilities for components, projects, fleets, and rigs |
| `homeboy deploy` | [deploy.md](deploy.md) | Deploy components to remote server |
| `homeboy harvest` | [harvest.md](harvest.md) | Recover remote component content into local Git history |
| `homeboy component` | [component.md](component.md) | Manage standalone component configurations |
| `homeboy config` | [config.md](config.md) | Manage global Homeboy configuration |
| `homeboy contract` | [contract.md](contract.md) | Inspect, export, validate, and normalize Homeboy contract metadata |
| `homeboy daemon` | [daemon.md](daemon.md) | Run the local-only HTTP API daemon |
| `homeboy extension` | [extension.md](extension.md) | Execute CLI-compatible extensions |
| `homeboy schedule` | [schedule.md](schedule.md) | Declare homeboy commands that run on a cadence |
| `homeboy status` | [status.md](status.md) | Actionable component status overview |
| `homeboy cleanup` | [cleanup.md](cleanup.md) | Remove declared reconstructable artifacts from managed worktrees |
| `homeboy git` | [git.md](git.md) | Git operations for components |
| `homeboy release` | [release.md](release.md) | Plan release workflows |
| `homeboy review` | [review.md](review.md) | Run scoped audit + lint + test umbrella against PR-style changes |
| `homeboy refactor` | [refactor.md](refactor.md) | Structural refactoring (rename terms across codebase) |
| `homeboy rig` | [rig.md](rig.md) | Manage local dev rigs (reproducible multi-component environments) |
| `homeboy runner` | [runner.md](runner.md) | Manage local and SSH execution runners |
| `homeboy source` | [source.md](source.md) | Inspect sealed source-package admissibility without staging resources |
| `homeboy runtime` | [runtime.md](runtime.md) | Inspect core-owned runtime helper assets |
| `homeboy worktree` | [worktree.md](worktree.md) | Manage component-backed task worktrees |
| `homeboy tunnel` | [tunnel.md](tunnel.md) | Manage private service tunnel declarations |
| `homeboy runs` | [runs.md](runs.md) | Inspect persisted observation runs, artifacts, and typed evidence projections |
| `homeboy self` | [self.md](self.md) | Inspect the active Homeboy binary; `self identity` reports its local build identity |
| `homeboy stack` | [stack.md](stack.md) | Manage stacks (combined-fixes branches built from base + cherry-picked PRs) |
| `homeboy api` | [api.md](api.md) | Make API requests to a project |
| `homeboy upgrade` | [upgrade.md](upgrade.md) | Upgrade Homeboy to the latest version |

## Commands shipping without help text

None. Every visible command declares a clap `about` string.
