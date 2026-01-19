# Agent Instructions (homeboy)

> **Note**: For project-wide architecture and future refactor plans, see the root [`../CLAUDE.md`](../CLAUDE.md).

This crate embeds its CLI documentation from `docs/` into the `homeboy` binary.

Key references:

- Embedded docs topics map to paths under `docs/` without the `.md` extension (e.g. `docs/commands/deploy.md` -> `commands/deploy`).
- Command output is machine-oriented and wrapped in a stable JSON envelope: [docs/json-output/json-output-contract.md](docs/json-output/json-output-contract.md).

When updating documentation, keep it concise and aligned with current implementation.

## Rust Testing (Release)

When validating changes in this workspace, always run tests using the **release target**:

- `cargo test --release`
- When running the CLI for validation, prefer `cargo run --release -p homeboy -- <args>`.

## Agent Workflow (Critical)

When working with Homeboy-managed repositories:

1. **Always start with `homeboy init`** - Understand context before any operations
2. **Use Homeboy commands for version changes** - `homeboy version set/bump`, never manual file edits
3. **Use Homeboy for builds** - `homeboy build <component>`, not manual build scripts
4. **Use Homeboy for deploys** - `homeboy deploy`, not manual rsync/scp

Never assume repo structure. Let `homeboy init` tell you:
- Which components exist
- What build commands are configured
- What version targets are managed
- What gaps need remediation
