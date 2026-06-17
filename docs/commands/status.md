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

## `unreleased_merges` — merged-but-not-live detection (read this)

`ready_to_deploy` (and `--ready`) compare the **local checkout** against the
latest tag, so they answer *"is my local git state ahead of the latest tag"*.
There is a higher-stakes inverse question they cannot answer:

> **"This PR is merged to `main` — is its code actually running on prod yet?"**

A merged PR has three states and only the last is live:

1. **merged-not-released** — merged on `origin/<default-branch>`, but no release
   tag covers it → the new ability/CLI/code **does not exist on prod**.
2. **released-not-deployed** — tagged, but the prod install runs an older
   version.
3. **live**.

Reading a merged-PR list alone produces a false "✅ shipped" for
merged-not-released code. The plain `homeboy status` summary now surfaces this as
`unreleased_merges`: per component, the count of commits on
`origin/<default-branch>` that are **past the latest release tag** (merge commits
excluded). Because it measures `origin/<default-branch>` (refreshed by the same
tag/branch fetch used for upstream drift), it is robust to a stale local checkout
— unlike `ready_to_deploy`, which depends on a fresh local HEAD.

When `unreleased_merges` is non-empty the JSON output includes an
`unreleased_merges_note` repeating the caveat. To check the **released →
deployed** axis (installed version vs latest tag), run `homeboy status <project>`
and look at `outdated`. Together, `unreleased_merges` (tag-vs-merged),
`ready_to_deploy` (local-vs-tag), and the project dashboard's `outdated`
(installed-vs-tag) close the merged → released → deployed chain. See issue #4996.

## Common filters

- `--full` — show the full workspace/context report
- `--uncommitted` — show only components with uncommitted changes
- `--needs-release` — show only components that need a release
- `--ready` — show only components in a clean release state (git state only — not a target diff)
- `--docs-only` — show only components with docs-only changes
- `--unreleased` — show only components carrying merged-but-unreleased work (commits on `origin/<default-branch>` past the latest release tag)
- `--all` — show all components regardless of current directory context
- `--outdated` — (project mode) show only components whose installed-on-target version is behind the latest release

## Related

- [component](component.md)
- [project](project.md)
- [triage](triage.md)
