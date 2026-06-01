# Remote Runner Extension Bootstrap

Remote Homeboy runners need the same extension set as the machine that dispatches
work to them. If a runner only has part of the standard set installed, Homeboy's
offload preflight rejects matching jobs before execution, for example when a
Rust component is sent to a runner that only has `nodejs` installed.

Use this page as the canonical bootstrap path for machines such as Homeboy Lab.

## Standard Extension Set

Install these extension IDs on general-purpose runners:

```text
nodejs rust wordpress go swift
```

This set covers the official project-type extensions in this repository. A
special-purpose runner may install fewer extensions, but the runner should only
advertise or receive workloads for extension IDs that are present on that host.

## Bootstrap A Remote Runner

From a checkout of this repository, run:

```bash
scripts/bootstrap-standard-extensions.sh --target homeboy-lab
```

Use any SSH target accepted by `ssh`:

```bash
scripts/bootstrap-standard-extensions.sh --target chubes@homeboy-lab
```

The script runs this idempotent install-and-verify loop on the target:

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id <extension-id>
homeboy extension show <extension-id>
```

After all installs pass, it prints `homeboy extension list` from the target so
the installed runner state is visible in the command output.

## Bootstrap The Local Machine

Omit `--target` to apply the same standard set locally:

```bash
scripts/bootstrap-standard-extensions.sh
```

Preview the exact target script without changing anything:

```bash
scripts/bootstrap-standard-extensions.sh --target homeboy-lab --dry-run
```

## Install A Subset

Use `--extensions` for narrow runners or repair passes:

```bash
scripts/bootstrap-standard-extensions.sh --target homeboy-lab --extensions "nodejs rust wordpress"
```

## GitHub Installs Vs Local Path Installs

Use the GitHub monorepo URL for normal runner bootstrap:

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust
```

GitHub installs give the runner a managed extracted copy under Homeboy's
extension directory and keep remote machines independent from a developer's
local checkout or short-lived worktree.

Use local path installs only while actively developing an extension on that
machine:

```bash
homeboy extension install ./homeboy-extensions/rust
```

Local path installs are linked installs. The active extension code is whatever
the symlink target points at, so they are easy to leave attached to stale or
temporary worktrees. Before debugging remote-runner behavior, inspect the target:

```bash
ssh homeboy-lab 'homeboy extension list && homeboy extension show rust'
ssh homeboy-lab 'readlink ~/.config/homeboy/extensions/rust || true'
```

If a linked install is stale, reinstall from the GitHub monorepo URL with the
matching `--id`.

## Custom Homeboy Binary Or Source

If the target exposes Homeboy under a different command path, pass `--homeboy`:

```bash
scripts/bootstrap-standard-extensions.sh \
    --target homeboy-lab \
    --homeboy /home/chubes/.local/bin/homeboy
```

To test an extension branch on a runner, pass a repository path or URL with
`--repo`. Prefer this only for deliberate extension development; general runners
should use the canonical GitHub URL.

```bash
scripts/bootstrap-standard-extensions.sh \
    --target homeboy-lab \
    --repo /home/chubes/src/homeboy-extensions \
    --extensions "rust"
```

## Verification Checklist

After bootstrap, verify the runner from the dispatching machine:

```bash
ssh homeboy-lab 'homeboy extension list'
ssh homeboy-lab 'homeboy extension show nodejs'
ssh homeboy-lab 'homeboy extension show rust'
ssh homeboy-lab 'homeboy extension show wordpress'
ssh homeboy-lab 'homeboy extension show go'
ssh homeboy-lab 'homeboy extension show swift'
```

For a real offload check, run a Homeboy command for a component that requires one
of the installed extensions and confirm preflight no longer reports the runner as
missing that extension.

## Refresh WP Codebox Cache

WordPress test and bench workloads use the runner-side WP Codebox cache at
`~/.cache/homeboy/wp-codebox/source` when WP Codebox is installed from source.
Refresh that cache before collecting lab evidence that must point at a known WP
Codebox revision:

```bash
wordpress/scripts/build/update-wp-codebox-cache.sh --runner homeboy-lab --ref main
```

The helper runs this sequence on the runner:

```text
git clone/fetch -> git reset --hard FETCH_HEAD -> npm install -> npm run build
```

On success it prints the resulting revision:

```text
WP Codebox cache SHA: <sha>
```

Use `--source`, `--ref`, and `--cache-dir` to target a fork, branch, tag, commit,
or non-default cache location. Omit `--runner` to run the same update locally.
