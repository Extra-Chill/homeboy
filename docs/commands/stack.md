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
    { "repo": "Automattic/studio", "number": 3120, "note": "proc_open cwd fix" },
    { "repo": "Automattic/studio", "number": 3211 }
  ]
}
```

`component_path` supports `~` and `${env.VAR}` expansion. `base` and `target` are split into `{ remote, branch }` so Homeboy can fetch and rebuild without reparsing slash-joined refs.

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

### `add-pr`

```sh
homeboy stack add-pr <stack-id> Automattic/studio 3120 --note "proc_open cwd fix"
```

Append a PR entry to the stack's `prs` array.

### `remove-pr`

```sh
homeboy stack remove-pr <stack-id> 3120
homeboy stack remove-pr <stack-id> 3120 --repo Automattic/studio
```

Remove a PR entry. Use `--repo` when the same number appears for multiple repos in one stack.

### `apply`

```sh
homeboy stack apply <stack-id>
```

Fetches the base, recreates the local target branch from the base, then cherry-picks every PR head in order. `apply` stops on the first conflict, aborts the in-progress pick, and prints a manual-resolution hint. It does not push.

### `status`

```sh
homeboy stack status <stack-id>
```

Read-only report combining upstream PR state from GitHub with local target-branch state. Use it to spot merged PRs, missing local picks, and review status without mutating the checkout.

### `sync`

```sh
homeboy stack sync <stack-id>
homeboy stack sync <stack-id> --dry-run
```

Rebuilds the target branch from the fresh base and removes PRs whose content is already in the base. `--dry-run` reports what would be dropped and picked without mutating the spec or target branch.

### `inspect`

```sh
homeboy stack inspect [component-id] [--base <ref>] [--repo <owner/name>] [--no-pr] [--path <path>]
```

Spec-less inspection of the current branch as a stack of commits over a base ref.

## GitHub dependency

`apply`, `status`, `sync`, and `inspect` PR lookup paths call the GitHub CLI (`gh`). Authenticate `gh` for private repositories before relying on stack reports.

## Related

- [rig](rig.md) — local dev environments that can reference stack IDs in component specs
- [git](git.md) — lower-level component-aware git primitives
