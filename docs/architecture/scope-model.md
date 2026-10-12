# Scope Model

Homeboy commands operate on different kinds of things. The shared scope model makes those things explicit so project/site workflows do not become the default mental model for every command.

## Scopes

| Scope | Meaning | Typical commands |
| --- | --- | --- |
| `component` | A source checkout or registered codebase. | `build`, `test`, `lint`, `release`, `changes`, `version`, `bench` |
| `project` | A runtime target: site, server, domain, base path, database/API context, and deploy destination. | `wp`, `db`, `logs`, `api`, `auth`, `deploy` |
| `rig` | A reproducible local environment composed from component paths, services, symlinks, and pipelines. | `rig up`, `rig check`, `rig down`, rig-pinned `bench`/`trace` |
| `fleet` | A group of projects/targets. | fleet status/check/exec, multi-target deploy |
| `workspace` | Everything Homeboy can discover locally across projects, rigs, standalone components, and the current checkout. | `triage workspace`, `runs`, global reports |
| `path` | An unregistered checkout operated on directly. | ad-hoc component/source workflows |

## Command Classes

Commands should describe which scope class they accept:

- **Component commands** operate on source code and should work with component IDs, paths, and CWD discovery without project registration.
- **Target commands** operate on a runtime/deploy destination and should require a project or fleet when the command writes to a site/server.
- **Environment commands** operate on rigs or stacks and should not require project ownership.
- **Workspace commands** aggregate evidence across scopes and should expose filters instead of silently inheriting active project state.

## Design Rules

- Keep `Project` as the existing config type for compatibility, but describe it as a runtime target in user-facing docs and errors.
- Standalone components and path-based components are first-class for local/source workflows.
- Rigs remain independent and portable; they may reference components by path without global registration.
- Project-required errors should appear only when a command truly needs a runtime target, such as deploy, WP-CLI, database, logs, auth, or API operations.
- Desktop and API consumers should use scope metadata to group tools by their natural operating context instead of assuming every tab belongs to the active project.

## Core API

The shared Rust model lives in `homeboy::scope`:

```rust
enum Scope {
    Component(String),
    Project(String),
    Fleet(String),
    Rig(String),
    Workspace,
    Path { path: String, component_id: Option<String> },
}
```

Use `resolve_scope_components()` when a feature needs to turn any supported scope into component references. Triage is the first consumer; additional commands can adopt the same resolver as they clarify their accepted scopes.
