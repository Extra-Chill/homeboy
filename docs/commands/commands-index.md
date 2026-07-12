# Commands index

- [api](api.md)
- [activity](activity.md) — unified active and recently finished Homeboy work
- [agent-task](agent-task.md)
- [api](api.md)
- [bench](bench.md) — performance benchmarks + p95 regression ratchet
- [cargo](cargo.md) — extension-provided Cargo routing when installed
- [check](check.md) — unified audit, lint, test, build, and review quality-gate entrypoint
- [cleanup](cleanup.md) — declared reconstructable artifact cleanup across managed worktrees
- [component](component.md)
- [config](config.md)
- [contract](contract.md) — core-owned contract registry, constants, exports, normalization, and command manifest
- [daemon](daemon.md) — local-only HTTP API daemon
- [db](db.md)
- [deploy](deploy.md)
- [deps](deps.md) — component dependency inspection and updates
- [extension](extension.md)
- [file](file.md) — remote file operations, downloads, copies, and syncs
- [fleet](fleet.md)
- [fuzz](fuzz.md) — generic fuzz workload discovery, execution, and evidence
- [git](git.md)
- [http](http.md) — generic proxied authenticated HTTP requests
- [logs](logs.md)
- [observe](observe.md) — passive live observation into trace timeline evidence
- [project](project.md)
- [report](report.md) — render reports from structured output artifacts
- [refactor](refactor.md) — structural refactoring, reference discovery, and undo snapshots
- [release](release.md) — local release pipeline
- [review](review.md) — scoped audit + lint + test umbrella for PR-style changes
- [rig](rig.md) — reproducible local dev environments ([spec](rig-spec.md))
- [runner](runner.md) — local and SSH execution runner registry
- [runtime](runtime.md) — narrow lookup for bundled core runtime helpers
- [runs](runs.md) — persisted observation runs, artifacts, postprocessing, and findings
- [server](server.md)
- [self](self.md) — active binary, install-signal, runtime drift, host resource inspection, and embedded docs
- [ssh](ssh.md)
- [stack](stack.md) — combined-fixes branches from base refs plus cherry-picked PRs
- [status](status.md) — actionable component overview
- [trace](trace.md) — black-box behavioral trace and evidence capture
- [triage](triage.md) — attention reports and watch utilities across components, projects, fleets, and rigs
- [tunnel](tunnel.md) — private service tunnel declarations
- [upgrade](upgrade.md)
- [worktree](worktree.md) — component-backed task worktree lifecycle

This list covers the top-level core CLI commands currently surfaced by `homeboy
--help` in this checkout. Hidden internal commands are omitted from this index.

Note: some extensions also expose additional top-level CLI commands at runtime
when installed. Extension command docs describe possible runtime-provided
commands rather than guaranteed core subcommands.

Agents and automation that need command safety metadata should read the recursive manifest with `homeboy contract manifest`.

Related:

- [Root command](../reference/cli/homeboy-root-command.md)
- [JSON output contract](../architecture/output-system.md) (global output envelope)
- [Embedded docs](../architecture/embedded-docs-topic-resolution.md)
- [Schema Reference](../reference/schemas/index.md) - JSON configuration schemas (component, project, server, extension)
- [Architecture](../architecture/) - System internals (API client, keychain, SSH, release pipeline, execution context)
- [Internals](../internals/index.md) - Contributing guides (architecture overview, config directory, error handling)
