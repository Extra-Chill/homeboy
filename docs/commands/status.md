# `homeboy status`

Show an actionable component status overview.

## Synopsis

```sh
homeboy status [PROJECT]
homeboy status --global
```

## Three modes

`homeboy status` behaves differently depending on whether you pass a project:

- **`homeboy status`** (no project) — a **git/workspace** summary of the
  components in scope. The `ready_to_deploy` list is **git-state only**.
- **`homeboy status <project>`** — a **target-accurate** dashboard that
  compares each component's installed-on-target version against its latest
  release tag and reports `current` / `outdated` / `pinned_current`.
- **`homeboy status --global`** — a bounded, local control-plane snapshot from
  any CWD. It reads the controller update cache, local daemon state, persisted
  runner sessions, bounded observation pages, and registered inventory counts.
  It does not fetch component remotes, inspect releases, or contact runners.

`--global` is the fast answer to "is this controller able to operate?" Its
payload is count-only for runners, activities, projects, and components, with
explicit drill-down commands. Runner freshness is intentionally reported as
unverified in this snapshot because proving it requires a runner-specific
inspection; use `homeboy runner status --full` for that bounded remote work.
When the local daemon is not admitting work, the snapshot returns
`homeboy daemon recover` as the repair command (dry-run by default).

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

## `controller` — is the binary reporting this actually current?

Every freshness signal above is measured *against the `homeboy` binary running
the command*. Nothing was measuring that binary. A controller can sit two minor
releases behind while it reports on everything else, and the report gives no
hint (issue #11483).

Both the summary and the project dashboard now carry a `controller` object:

```json
{
  "controller": {
    "status": "behind_minor",
    "stale": true,
    "escalated": true,
    "running_version": "0.327.0",
    "build_identity": "homeboy 0.327.0+ed33954781a9",
    "git_commit": "ed33954781a9",
    "latest_version": "0.329.1",
    "minor_releases_behind": 2,
    "checked_at": 1754320550,
    "cache_age_secs": 900,
    "detail": "STALE: homeboy 0.327.0+ed33954781a9 is 2 minor release(s) behind v0.329.1 — run `homeboy upgrade`",
    "remediation": "homeboy upgrade"
  }
}
```

- `status` is one of `current`, `ahead`, `behind_patch`, `behind_minor`,
  `behind_major`, `unknown`. It is emitted whether or not the controller is
  stale, so a reader can tell *"checked, current"* from *"never checked"*
  instead of reading silence as health.
- `escalated` is `true` past **more than one minor release behind** (or any
  major). At that distance the controller is dispatching work whose behavior it
  may not model correctly — the controller-side analogue of the runner version
  checks. When it is set, the `detail` line is prefixed `STALE:` and is also
  written to stderr.
- `unknown` covers an offline host, a first run, and an update check disabled
  via `HOMEBOY_NO_UPDATE_CHECK` or `homeboy config set /update_check false`. An
  unestablished verdict is never reported as `current` and never warns.

**Cost.** No network call is made by `status`. The latest published release is
read from the same daily cache the startup update check already maintains, so
the whole surface is one small file read per command and at most one network
call per day. A failure to check degrades to `unknown`; it never fails a
command.

**What is not reported.** The commit delta (`215 commits behind`) needs a source
checkout with a fresh `origin/main`, which a packaged install does not have. The
release delta is reported instead, with the running build's embedded git commit
alongside it so anyone holding a checkout can compute the commit delta
themselves.

## Common filters

- `--full` — show the full workspace/context report
- `--uncommitted` — show only components with uncommitted changes
- `--needs-release` — show only components that need a release
- `--ready` — show only components in a clean release state (git state only — not a target diff)
- `--docs-only` — show only components with docs-only changes
- `--unreleased` — show only components carrying merged-but-unreleased work (commits on `origin/<default-branch>` past the latest release tag)
- `--all` — show all components regardless of current directory context
- `--global` — show the local, count-only control-plane snapshot from any CWD; no component remote fetches or runner probes
- `--outdated` — (project mode) show only components whose installed-on-target version is behind the latest release
- `--timings` — emit phase progress to stderr and include phase timings in JSON, useful when diagnosing slow status runs

## Scope selectors

Status takes the shared scope selectors, which are mutually exclusive:

- `--project <ID>` — the project dashboard, same as the positional `[PROJECT]`
- `--component <ID>` — summarize one registered component
- `--fleet <ID>` / `--rig <ID>` / `--workspace` — summarize every component the
  scope resolves to
- `--path <PATH>` — inspect this checkout instead of the registered component
  path; composes with the positional target

## Related

- [component](component.md)
- [project](project.md)
- [triage](triage.md)
