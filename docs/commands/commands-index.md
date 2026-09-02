<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the Clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference -->

# Commands index

- [activity](activity.md)
- [agent-task](agent-task.md)
- [api](api.md)
- [bench](bench.md)
- [cargo](cargo.md)
- [cleanup](cleanup.md)
- [component](component.md)
- [config](config.md)
- [contract](contract.md)
- [daemon](daemon.md)
- [db](db.md)
- [deferred-workload](deferred-workload.md)
- [deploy](deploy.md)
- [deps](deps.md)
- [extension](extension.md)
- [file](file.md)
- [fleet](fleet.md)
- [fuzz](fuzz.md)
- [git](git.md)
- [harvest](harvest.md)
- [logs](logs.md)
- [project](project.md)
- [refactor](refactor.md)
- [release](release.md)
- [review](review.md)
- [rig](rig.md)
- [runner](runner.md)
- [runs](runs.md)
- [runtime](runtime.md)
- [schedule](schedule.md)
- [self](self.md)
- [server](server.md)
- [source](source.md)
- [ssh](ssh.md)
- [stack](stack.md)
- [status](status.md)
- [topology](topology.md)
- [trace](trace.md)
- [triage](triage.md)
- [tunnel](tunnel.md)
- [upgrade](upgrade.md)
- [worktree](worktree.md)

This list covers the top-level core CLI commands currently surfaced by `homeboy --help` in this checkout. Hidden internal commands are omitted from this index.

Agents and automation that need command safety metadata should read the recursive manifest with `homeboy contract manifest`.

Every command page above is hand-written narrative. The exhaustive, always-current synopsis/flag/subcommand surface for each command is generated from Clap into [the CLI reference](../reference/cli/commands/index.md).

Related:

- [Root command](../reference/cli/homeboy-root-command.md)
- [JSON output contract](../architecture/output-system.md) (global output envelope)
- [Embedded docs](../architecture/embedded-docs-topic-resolution.md)
- [Schema Reference](../reference/schemas/index.md) - JSON configuration schemas (component, project, server, extension)
- [Architecture](../architecture/) - System internals (API client, keychain, SSH, release pipeline, execution context)
- [Internals](../internals/index.md) - Contributing guides (architecture overview, config directory, error handling)
