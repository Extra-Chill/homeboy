# Release Pipeline System

Homeboy release automation is a local-first planner/executor for component
releases. It turns component metadata, conventional commits, extension release
actions, and git state into a reviewable release plan.

## Current Model

The release command is intentionally not a generic shell workflow runner. Core
owns the release graph and safety checks; extensions contribute domain-specific
prepare, package, and publish behavior through release actions declared in their
manifests.

```text
component config + git history
        |
        v
release planner
        |
        +-- core steps: preflight, changelog, version, git, deploy, cleanup
        |
        +-- extension actions: prepare, package, publish.<target>
        v
structured release plan + execution results
```

## Core Step Kinds

Common executable step kinds include:

- `preflight.default_branch`
- `preflight.git_identity`
- `preflight.working_tree`
- `preflight.remote_sync`
- `preflight.bump_policy`
- `preflight.lint`
- `preflight.test`
- `preflight.changelog_bootstrap`
- `changelog.finalize`
- `version`
- `git.commit`
- `package`
- `artifacts.inventory`
- `git.tag`
- `git.push`
- `github.release`
- `deploy`
- `cleanup`
- `post_release`
- `publish.<target>`

Some preflight and changelog planning steps are plan-only. They appear in the
release plan for visibility but are not executed as mutating steps.

## Extension Boundary

Extensions should own ecosystem-specific release semantics:

- how to package a Rust crate, WordPress plugin, Node project, or Swift artifact
- how to publish to GitHub, Homebrew, crates.io, npm, or another registry
- how to run platform-specific prepare checks before packaging
- how to report skipped or auth-blocked publish attempts in a structured way

Core should own generic sequencing, state, git operations, output contracts, and
failure handling.

## Commands

Preview a release without executing mutating steps:

```bash
homeboy release --dry-run <component_id>
```

Execute the release plan:

```bash
homeboy release <component_id>
```

Useful companion commands:

```bash
homeboy changes <component_id>
homeboy version show <component_id>
homeboy changelog show <component_id>
```

## Execution Behavior

Release execution:

1. Resolves the component and extension context.
2. Builds a release plan from git history, component metadata, and extension
   capabilities.
3. Runs executable steps in dependency order.
4. Stops on failure and returns structured step results.
5. Leaves plan-only steps visible for review without executing them.

Release requires a clean working tree except for files the release process owns,
such as version targets and changelog targets.

## Historical Terminology

Older docs and changelog entries may mention `extension_run`,
`extension_action`, or generic `extension.run` release steps. Treat those as
historical terminology unless the current release planner emits those step kinds.
Current release execution is built around known core step kinds and declared
extension release actions.

## Related

- [Release command](../commands/release.md)
- [Component schema](../reference/schemas/component-schema.md)
- [Extension manifest schema](../reference/schemas/extension-manifest-schema.md)
