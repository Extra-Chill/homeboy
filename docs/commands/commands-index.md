# Commands index

- [api](api.md)
- [audit](audit.md) — code convention drift and structural analysis
- [auth](auth.md)
- [build](build.md)
- [changelog](changelog.md)
- [changes](changes.md)
- [component](component.md)
- [config](config.md)
- [db](db.md)
- [deploy](deploy.md)
- [docs](docs.md) — topic display, audit, map, generate
- [extension](extension.md)
- [file](file.md)
- [fleet](fleet.md)
- [git](git.md)
- [init](init.md) — deprecated alias for `status --full`
- [lint](lint.md)
- [list](list.md)
- [logs](logs.md)
- [project](project.md)
- [refactor](refactor.md)
- [release](release.md) — local release pipeline
- [server](server.md)
- [ssh](ssh.md)
- status — actionable component overview (`--uncommitted`, `--needs-bump`, `--ready`, `--docs-only`, `--all`, `--full`)
- [supports](supports.md) — machine-readable CLI capability checks
- [test](test.md)
- transfer — transfer files between servers (`<source> <destination>`, supports `-r`, `-c`, `--dry-run`, `--exclude`)
- [upgrade](upgrade.md)
- [version](version.md)

This list covers built-in CLI commands.

Note: some extensions also expose additional top-level CLI commands at runtime (loaded from installed extensions).

Related:

- [Root command](../cli/homeboy-root-command.md)
- [JSON output contract](../architecture/output-system.md) (global output envelope)
- [Embedded docs](../architecture/embedded-docs-topic-resolution.md)
- [Schema Reference](../schemas/) - JSON configuration schemas (component, project, server, extension)
- [Architecture](../architecture/) - System internals (API client, keychain, SSH, release pipeline, execution context)
- [Developer Guide](../developer-guide/) - Contributing guides (architecture overview, config directory, error handling)
