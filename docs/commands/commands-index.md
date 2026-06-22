# Commands index

- [api](api.md)
- [agent-task](agent-task.md)
- [audit](audit.md) — code convention drift and structural analysis
- [audit-baseline](audit-baseline.md) — deterministic audit baseline refresh workflow
- [auth](auth.md)
- [bench](bench.md) — performance benchmarks + p95 regression ratchet
- [build](build.md) — local build quality gate
- [cargo](cargo.md) — extension-provided Cargo routing when installed
- [changelog](changelog.md)
- [changes](changes.md)
- [ci](ci.md) — CI reproduction profiles and shallow CI surface discovery
- [cleanup](cleanup.md) — declared reconstructable artifact cleanup across managed worktrees
- [component](component.md)
- [config](config.md)
- [daemon](daemon.md) — local-only HTTP API daemon
- [db](db.md)
- [deploy](deploy.md)
- [deps](deps.md) — component dependency inspection and updates
- [doctor](doctor.md) — local diagnostics for runtime and resource health
- [docs](docs.md) — embedded topic display and codebase map generation
- [extension](extension.md)
- [file](file.md) — remote file operations, downloads, copies, and syncs
- [fleet](fleet.md)
- [fuzz](fuzz.md) — generic fuzz workload discovery, execution, and evidence
- [git](git.md)
- [http](http.md) — generic proxied authenticated HTTP requests
- [issues](issues.md) — reconcile findings against issue trackers
- [lint](lint.md)
- [logs](logs.md)
- [observe](observe.md) — passive live observation into trace timeline evidence
- [project](project.md)
- [report](report.md) — render reports from structured output artifacts
- [refactor](refactor.md)
- [release](release.md) — local release pipeline
- [refs](refs.md)
- [review](review.md) — scoped audit + lint + test umbrella for PR-style changes
- [rig](rig.md) — reproducible local dev environments ([spec](rig-spec.md))
- [runner](runner.md) — local and SSH execution runner registry
- [runtime](runtime.md) — narrow lookup for bundled core runtime helpers
- [runs](runs.md) — persisted observation runs and artifacts
- [server](server.md)
- [self](self.md) — active binary and install-signal inspection
- [ssh](ssh.md)
- [stack](stack.md) — combined-fixes branches from base refs plus cherry-picked PRs
- [status](status.md) — actionable component overview
- [test](test.md)
- [trace](trace.md) — black-box behavioral trace and evidence capture
- [triage](triage.md) — attention reports and watch utilities across components, projects, fleets, and rigs
- [tunnel](tunnel.md) — private service tunnel declarations
- [undo](undo.md) — restore or manage write-operation snapshots
- [upgrade](upgrade.md)
- [version](version.md)
- [worktree](worktree.md) — component-backed task worktree lifecycle
- [wp](wp.md) — extension-provided WP-CLI routing when installed

This list covers the top-level core CLI commands currently surfaced by `homeboy
--help` in this checkout. Hidden compatibility aliases such as `lab` are
documented but omitted from this index.

Note: some extensions also expose additional top-level CLI commands at runtime
when installed. Extension command docs, including `cargo` and `wp`, describe
possible runtime-provided commands rather than guaranteed core subcommands.

Agents and automation that need command safety metadata should read the recursive manifest with `homeboy list --json`. The `list` command is hidden and deprecated as a help alias, but `list --json` remains the compatibility entry point for the safety manifest.

Related:

- [Root command](../cli/homeboy-root-command.md)
- [JSON output contract](../architecture/output-system.md) (global output envelope)
- [Embedded docs](../architecture/embedded-docs-topic-resolution.md)
- [Schema Reference](../schemas/) - JSON configuration schemas (component, project, server, extension)
- [Architecture](../architecture/) - System internals (API client, keychain, SSH, release pipeline, execution context)
- [Developer Guide](../developer-guide/) - Contributing guides (architecture overview, config directory, error handling)
