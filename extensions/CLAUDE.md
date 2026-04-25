## Extension Version Management

When modifying extension manifests (e.g., `nodejs.json`, `wordpress.json`), always manage versions independently. Increment the `version` field (following semantic versioning) for any breaking changes, feature additions, or bug fixes. This ensures:

- Installed copies can be detected as outdated.
- Users receive proper update prompts via `homeboy init`.
- Compatibility with automated extension updating systems.

Example: If removing `cli.tool` from the Node.js extension, bump version from "3.0.0" to "3.1.0" — Homeboy generates the changelog from commit messages at release time, do not edit `CHANGELOG.md` manually.

Failure to version independently may lead to stale cached extensions causing incorrect behavior (e.g., persistent CLI suggestions).

## Scope

This repo holds **project-type primitives** for Homeboy: discover, audit, lint, test, refactor, release, deploy. Anything that runs as a long-lived process, talks to a remote API at runtime, or scrapes/automates external services belongs in [Sweatpants](https://github.com/Extra-Chill/sweatpants), not here.
