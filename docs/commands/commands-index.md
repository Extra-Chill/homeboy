# Commands index

- [api](api.md)
- [audit](audit.md) — code convention drift and structural analysis
- [auth](auth.md)
- [bench](bench.md) — performance benchmarks + p95 regression ratchet
- [build](build.md)
- [cargo](cargo.md) — run Cargo commands via Rust extension routing
- [changelog](changelog.md)
- [changes](changes.md)
- [component](component.md)
- [config](config.md)
- [daemon](daemon.md) — local-only HTTP API daemon
- [db](db.md)
- [deploy](deploy.md)
- [deps](deps.md) — component dependency inspection and updates
- [docs](docs.md) — embedded topic display and codebase map generation
- [extension](extension.md)
- [file](file.md) — remote file operations, downloads, uploads, copies, and syncs
- [fleet](fleet.md)
- [git](git.md)
- [issues](issues.md) — reconcile findings against issue trackers
- [lint](lint.md)
- [list](list.md)
- [logs](logs.md)
- [project](project.md)
- [report](report.md) — render reports from structured output artifacts
- [refactor](refactor.md)
- [release](release.md) — local release pipeline
- [review](review.md) — scoped audit + lint + test umbrella for PR-style changes
- [rig](rig.md) — reproducible local dev environments ([spec](rig-spec.md))
- [server](server.md)
- [self](self.md) — active binary and install-signal inspection
- [ssh](ssh.md)
- [stack](stack.md) — combined-fixes branches from base refs plus cherry-picked PRs
- [status](status.md) — actionable component overview
- [test](test.md)
- [triage](triage.md) — read-only attention report across components, projects, fleets, and rigs
- [undo](undo.md) — restore or manage write-operation snapshots
- [upgrade](upgrade.md)
- [version](version.md)
- [wp](wp.md) — run WP-CLI commands via WordPress extension routing

This list covers the top-level CLI commands currently surfaced by `homeboy --help` in this checkout.

Note: some extensions also expose additional top-level CLI commands at runtime (loaded from installed extensions).

Related:

- [Root command](../cli/homeboy-root-command.md)
- [JSON output contract](../architecture/output-system.md) (global output envelope)
- [Embedded docs](../architecture/embedded-docs-topic-resolution.md)
- [Schema Reference](../schemas/) - JSON configuration schemas (component, project, server, extension)
- [Architecture](../architecture/) - System internals (API client, keychain, SSH, release pipeline, execution context)
- [Developer Guide](../developer-guide/) - Contributing guides (architecture overview, config directory, error handling)
