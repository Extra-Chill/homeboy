# Working on Homeboy

- `crates/` and `src/` contain the CLI and orchestration core.
- `extensions/` contains independently installable project, deployment, and integration extensions and their shared helpers.
- `action/` contains the GitHub Action wrapper for CI.
- `desktop/` contains the macOS client.

Versions are owned by `homeboy release`, which updates each component’s declared version targets and changelog. Never hand-edit version fields or release-generated `CHANGELOG.md` entries.

See the [engineering rules](docs/internals/developer-guide/engineering-rules.md) for contributor guidance.
