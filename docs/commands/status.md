# `homeboy status`

Show an actionable component status overview.

## Synopsis

```sh
homeboy status [PROJECT]
```

## Two modes

`homeboy status` behaves differently depending on whether you pass a project:

- **`homeboy status`** (no project) — a **git/workspace** summary of the
  components in scope. The `ready_to_deploy` list is **git-state only**.
- **`homeboy status <project>`** — a **target-accurate** dashboard that
  compares each component's installed-on-target version against its latest
  release tag and reports `current` / `outdated` / `pinned_current`.

## `ready_to_deploy` is git-state only (read this)

In the plain `homeboy status` summary, `ready_to_deploy` lists components that
are in a **clean release state**: no uncommitted changes and no commits since
the last version tag — i.e. they *have a release tag that could be deployed*.

It does **not** mean the deploy target is behind. A component can be
`ready_to_deploy` while the target already runs that exact version, so acting on
the list blindly re-deploys components that are already live (a phantom
backlog). When `ready_to_deploy` is non-empty, the JSON output includes a
`ready_to_deploy_note` field repeating this caveat.

For the question *"what actually needs deploying right now?"*, run
`homeboy status <project>` and look at the components reported as `outdated`
(installed version != latest release tag). See issue #4588.

## Common filters

- `--full` — show the full workspace/context report
- `--uncommitted` — show only components with uncommitted changes
- `--needs-release` — show only components that need a release
- `--ready` — show only components in a clean release state (git state only — not a target diff)
- `--docs-only` — show only components with docs-only changes
- `--all` — show all components regardless of current directory context
- `--outdated` — (project mode) show only components whose installed-on-target version is behind the latest release

## Related

- [component](component.md)
- [project](project.md)
- [triage](triage.md)
