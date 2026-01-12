# Commands index

- [build](build.md)
- [changelog](changelog.md)
- [component](component.md)
- [error](error.md)
- [config](config.md)
- [context](context.md)
- [db](db.md)

- [deploy](deploy.md)
- [docs](docs.md)
- [doctor](doctor.md)
- [file](file.md)
- [git](git.md)
- [logs](logs.md)
- [module](module.md)
- [homeboy-init](homeboy-init.md)
- [project](project.md)
- [server](server.md)
- [ssh](ssh.md)
- [version](version.md)

Module-provided CLI commands (from installed modules that define `cli.tool`) also appear as top-level commands (for example: `wp`, `pm2`).

These commands are generated at runtime from installed module manifests (loaded via `homeboy_core::module::load_all_modules`).

Related:

- [Root command](../cli/homeboy-root-command.md)
- [JSON output contract](../json-output/json-output-contract.md) (global output envelope)
- [Embedded docs](../embedded-docs/embedded-docs-topic-resolution.md)
