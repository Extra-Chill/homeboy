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
scripts/bootstrap-standard-extensions.sh --target example-runner
```

Use any SSH target accepted by `ssh`:

```bash
scripts/bootstrap-standard-extensions.sh --target operator@example-runner
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
scripts/bootstrap-standard-extensions.sh --target example-runner --dry-run
```

## Repair Existing Installs

Default bootstrap is non-destructive: it installs missing extensions and leaves
existing installs alone. If a runner extension is already linked to a stale or
dirty checkout, repair it explicitly with `--replace-existing`:

```bash
scripts/bootstrap-standard-extensions.sh \
    --target example-runner \
    --extensions "wordpress" \
    --replace-existing
```

This runs `homeboy extension install <repo> --id <extension-id> --replace` on the
runner. For linked installs, Homeboy removes only the installed symlink and
preserves the linked source checkout, then installs a managed extracted copy from
the configured repository URL. For copied installs, replacement removes the
managed installed copy, so use this flag only for an intentional repair or
refresh pass.

## Install A Subset

Use `--extensions` for narrow runners or repair passes:

```bash
scripts/bootstrap-standard-extensions.sh --target example-runner --extensions "nodejs rust wordpress"
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
ssh example-runner 'homeboy extension list && homeboy extension show rust'
ssh example-runner 'readlink ~/.config/homeboy/extensions/rust || true'
```

If a linked install is stale, reinstall from the GitHub monorepo URL with the
matching `--id` and `--replace`, or use the bootstrap repair mode:

```bash
scripts/bootstrap-standard-extensions.sh \
    --target example-runner \
    --extensions "rust" \
    --replace-existing
```

## Custom Homeboy Binary Or Source

If the target exposes Homeboy under a different command path, pass `--homeboy`:

```bash
scripts/bootstrap-standard-extensions.sh \
    --target example-runner \
    --homeboy /home/operator/.local/bin/homeboy
```

To test an extension branch on a runner, pass a repository path or URL with
`--repo`. Prefer this only for deliberate extension development; general runners
should use the canonical GitHub URL.

```bash
scripts/bootstrap-standard-extensions.sh \
    --target example-runner \
    --repo /home/operator/src/homeboy-extensions \
    --extensions "rust"
```

## Verification Checklist

After bootstrap, verify the runner from the dispatching machine:

```bash
ssh example-runner 'homeboy extension list'
ssh example-runner 'homeboy extension show nodejs'
ssh example-runner 'homeboy extension show rust'
ssh example-runner 'homeboy extension show wordpress'
ssh example-runner 'homeboy extension show go'
ssh example-runner 'homeboy extension show swift'
```

For a real offload check, run a Homeboy command for a component that requires one
of the installed extensions and confirm preflight no longer reports the runner as
missing that extension.

## Refresh WP Codebox Cache

WordPress test and bench workloads use the runner-side WP Codebox cache at
`~/.cache/homeboy/wp-codebox/source` when WP Codebox is installed from source.
Homeboy extension adapters do not inspect that cache to discover private Codebox
package files; runners should expose public Codebox package exports or set
`HOMEBOY_WP_CODEBOX_CORE_MODULE` explicitly when a public module is not on
Node's resolution path.
Refresh that cache before collecting lab evidence that must point at a known WP
Codebox revision:

```bash
wordpress/scripts/build/update-wp-codebox-cache.sh --runner example-runner --ref main
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
