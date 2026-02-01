## Module Version Management

When modifying module manifests (e.g., `github.json`), always manage versions independently. Increment the `version` field (following semantic versioning) for any breaking changes, feature additions, or bug fixes. This ensures:

- Installed copies can be detected as outdated.
- Users receive proper update prompts via `homeboy init`.
- Compatibility with automated module updating systems.

Example: If removing `cli.tool` from GitHub module, bump version from "1.0.0" to "1.1.0" and document in changelog.

Failure to version independently may lead to stale cached modules causing incorrect behavior (e.g., persistent CLI suggestions).