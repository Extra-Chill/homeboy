# `homeboy stack`

Manage **combined-fixes branches** as JSON specs instead of hand-maintained cherry-pick lists.

## Synopsis

```sh
homeboy stack <COMMAND>
```

Stack specs live at `~/.config/homeboy/stacks/<id>.json`. A stack declares one checkout path, a base ref to rebuild from, a target branch to materialize, and an ordered list of GitHub PRs to cherry-pick.

## Spec format

```jsonc
{
  "id": "studio-combined",
  "description": "Studio dev/combined-fixes branch",
  "component": "studio",
  "component_path": "~/Developer/studio",
  "base": { "remote": "origin", "branch": "trunk" },
  "target": { "remote": "fork", "branch": "dev/combined-fixes" },
  "prs": [
    { "repo": "example-org/studio", "number": 3120, "note": "proc_open cwd fix" },
    { "repo": "example-org/studio", "number": 3211 }
  ]
}
```

`component_path` supports `~` and `${env.VAR}` expansion. `base` and `target` are split into `{ remote, branch }` so Homeboy can fetch and rebuild without reparsing slash-joined refs.

Installed specs can also declare provenance and compatibility evidence:

```jsonc
{
  "provenance": { "source": "https://github.com/example-org/stack-specs", "revision": "abc123" },
  "requirements": {
    "compatible_bases": [{ "remote": "origin", "branch": "trunk" }]
  }
}
```

`compatible_bases` is an explicit statement that this stack composition can replace a stack using one of those base refs. Homeboy does not infer compatibility from branch names.

## Subcommands

### `list`

```sh
homeboy stack list
```

List installed stack specs.

### `show`

```sh
homeboy stack show <stack-id>
```

Print the resolved stack spec.

### `create`

```sh
homeboy stack create <stack-id> \
  --component studio \
  --component-path ~/Developer/studio \
  --base origin/trunk \
  --target fork/dev/combined-fixes \
  --description "Studio combined fixes"
```

Create a spec file under `~/.config/homeboy/stacks/`.

### `candidates`

```sh
homeboy stack candidates <stack-id>
```

Lists installed alternatives for the same component and repository without changing the selected stack. Candidates are ordered by declared base compatibility, then count of overlapping PR coordinates, then stack ID. Each JSON candidate includes its declared provenance (or `null` when absent), target, requirements, overlap evidence, and an exact `selection_command`. Run that command explicitly; discovery never switches or recreates a target branch.

### `add-pr`

```sh
homeboy stack add-pr <stack-id> example-org/studio 3120 --note "proc_open cwd fix"
```

Append a PR entry to the stack's `prs` array.

### `remove-pr`

```sh
homeboy stack remove-pr <stack-id> 3120
homeboy stack remove-pr <stack-id> 3120 --repo example-org/studio
```

Remove a PR entry. Use `--repo` when the same number appears for multiple repos in one stack.

### `apply`

```sh
homeboy stack apply <stack-id>
homeboy stack apply <stack-id> --abort-on-conflict
```

Fetches the base, recreates the local target branch from the base, then cherry-picks every PR head in order. It does not push.

On the first conflict `apply` stops and **leaves the conflicted cherry-pick in the checkout** so it can actually be resolved. The error names the two commands that work from that state — `git -C <path> cherry-pick --continue` after staging the resolution, or `git -C <path> cherry-pick --abort` to bail out — plus the homeboy command to re-run afterwards.

Before recreating the target, `apply` reads installed candidate declarations after fetching the requested base. If a cherry-pick subsequently conflicts, the structured error includes the deterministic candidate list and explicit selection commands. This is advisory evidence only: Homeboy fails closed and never switches to a candidate automatically.

`--abort-on-conflict` runs `git cherry-pick --abort`, restores the target to its pre-rebuild tip, and reports a clean tree. Use it for unattended runs where a paused checkout is worse than an aborted rebuild.

While a cherry-pick is still paused, `apply`, `rebase`, and `sync` refuse to rebuild the target branch rather than clobber an in-progress resolution.

Safety manifest metadata marks `apply` and `rebase` as explicit branch-mutation
actions. Use `status` for read-only inspection and `sync --dry-run` for the
available planning path before mutating a stack branch.

### `status`

```sh
homeboy stack status <stack-id>
```

Read-only report combining upstream PR state from GitHub with local target-branch state. It does not clone, fetch, or mutate a checkout.

When `component_path` is absent, status still reports each PR's GitHub metadata. Local branch, ahead, and behind evidence is omitted and `local_evidence_unavailable` explains the missing path with recovery commands. Per-PR `local_state` is `unknown` in that case. Use it to spot merged PRs and review status before materializing a checkout.

### `sync`

```sh
homeboy stack sync <stack-id>
homeboy stack sync <stack-id> --dry-run
homeboy stack sync <stack-id> --abort-on-conflict
```

Rebuilds the target branch from the fresh base and removes PRs whose content is already in the base. `--dry-run` reports what would be dropped and picked without mutating the spec or target branch. Conflict handling — and `--abort-on-conflict` — match [`apply`](#apply).

### `inspect`

```sh
homeboy stack inspect [component-id] [--base <ref>] [--repo <owner/name>] [--no-pr] [--path <path>]
```

Spec-less inspection of the current branch as a stack of commits over a base ref.

## GitHub dependency

`apply`, `status`, `sync`, and `inspect` PR lookup paths call the GitHub CLI (`gh`). Authenticate `gh` for private repositories before relying on stack reports.

`push` is classified as an explicit remote publication action in the safety
manifest. It does not currently expose a dry-run contract.

## Related

- [rig](rig.md) — local dev environments that can reference stack IDs in component specs
- [git](git.md) — lower-level component-aware git primitives
