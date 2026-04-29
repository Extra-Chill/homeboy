# `homeboy rig`

Manage local dev **rigs**: reproducible multi-component environments that can start services, prepare checkouts, sync combined-fixes stacks, run health checks, and install desktop launchers.

## Synopsis

```sh
homeboy rig <COMMAND>
```

## Overview

A rig is a named environment spec. Local rig configs live at `~/.config/homeboy/rigs/<id>.json`; package-installed rigs are linked or copied into that same flat config path so every command uses one lookup model.

Use rigs for local environments that otherwise become wiki runbooks: Studio plus Playground, WordPress core plus Gutenberg, a sandbox plus tunnel, or any setup where multiple repositories and services must be moved together.

```text
rig package / local spec
        |
        v
~/.config/homeboy/rigs/<id>.json
        |
        +-- rig sync   -> declared component stacks
        +-- rig up     -> services, git/build/patch/check steps, symlinks
        +-- rig check  -> health/preflight report
        +-- rig down   -> teardown
        +-- rig app    -> optional desktop launcher
```

## Command Surface

| Command | Purpose |
|---|---|
| `list` | List declared rigs. |
| `show <id>` | Print one rig spec as JSON. |
| `up <id>` | Run the rig's `up` pipeline and materialize the environment. |
| `check <id>` | Run the rig's `check` pipeline and report all failures. |
| `down <id>` | Run the rig's `down` pipeline and stop managed services. |
| `sync <id>` | Sync every stack declared by the rig's components. |
| `status <id>` | Show running services and last `up` / `check` state. |
| `install <source>` | Install rigs from a local package path or git URL. |
| `update [id]` | Pull and refresh git-backed installed rig packages. |
| `sources ...` | Inspect or remove installed rig source packages. |
| `app ...` | Install, update, or remove a rig's desktop launcher. |

All subcommands support `--output <path>` for structured JSON output in addition to stdout.

## Local Lifecycle

### `list`

```sh
homeboy rig list
```

Lists every rig config currently visible under the Homeboy config directory.

### `show`

```sh
homeboy rig show studio
```

Prints the resolved rig spec. This is the fastest way to inspect a package-installed rig without opening the linked source file manually.

### `up`

```sh
homeboy rig up studio
```

Runs the `up` pipeline. Mutating rig commands acquire a resource lease first when the rig declares `resources`, so two active rig commands cannot mutate the same declared paths, ports, process patterns, or exclusive tokens at once.

The pipeline can start services, run typed `git` / `build` / `extension` / `stack` steps, apply idempotent local patches, create symlinks/shared paths, and run checks.

### `check`

```sh
homeboy rig check studio
```

Runs the `check` pipeline and reports every failing step instead of stopping at the first failure. Use this as the preflight for a local environment.

### `down`

```sh
homeboy rig down studio
```

Runs the `down` pipeline, cleans rig-owned shared paths, and stops declared services. `external` services are adopted rather than spawned, so `down` can recycle stale daemons that were started by another tool.

### `status`

```sh
homeboy rig status studio
```

Reports service PIDs, stale service state, and last recorded `up` / `check` timestamps from the rig state directory.

## Package Lifecycle

### `install`

```sh
homeboy rig install https://github.com/chubes4/homeboy-rigs.git//packages/studio --id studio
homeboy rig install ./packages/studio
homeboy rig install https://github.com/chubes4/homeboy-rigs.git//packages --all
```

Installs rigs from a local directory or git-backed package. Package discovery accepts either a single `rig.json` or a package layout with `rigs/<id>/rig.json`. If the selected package also contains `stacks/*.json`, those stack specs are installed alongside the rig.

Git sources may include a Terraform-style `repo.git//subpath` selector. Homeboy clones the package root, records source metadata, and discovers rigs from the selected subpath. Local package sources are linked in place and are updated outside Homeboy.

### `update`

```sh
homeboy rig update studio
homeboy rig update --all
```

Updates git-backed package sources with `git pull`, then refreshes the installed rig and stack config links. Local linked sources are skipped by `--all` and error for single-rig updates because the source is already the editable checkout.

If the user replaced an installed config file with their own file, update preserves that user-owned config and reports it as skipped instead of overwriting it.

### `sources`

```sh
homeboy rig sources list
homeboy rig sources remove chubes4-homeboy-rigs
```

`sources list` groups installed rigs and stacks by package source, package path, revision, and ownership. `sources remove` removes Homeboy-owned config links and metadata for one source package; it also removes cloned git packages, while linked local package directories are left in place.

## Stack Sync

```sh
homeboy rig sync studio
homeboy rig sync studio --dry-run
```

`rig sync` finds every component with a `stack` field and delegates to `homeboy stack sync <stack-id>`. This is the rig-level entry point for keeping combined-fixes branches current before a local environment is brought up.

```jsonc
{
  "components": {
    "studio": {
      "path": "~/Developer/studio",
      "stack": "studio-combined"
    },
    "playground": {
      "path": "~/Developer/wordpress-playground",
      "stack": "playground-combined"
    }
  }
}
```

`rig up` does not sync stacks implicitly. If stack sync should be part of an `up` pipeline, add an explicit `stack` pipeline step.

## App Launchers

```sh
homeboy rig app install studio
homeboy rig app update studio --dry-run
homeboy rig app uninstall studio
```

`rig app` manages an optional desktop launcher declared in `app_launcher`. The current implementation generates a macOS `.app` wrapper that runs rig preflight, runs `rig up`, and opens the target app when the rig is ready. Use `--dry-run` to preview generated paths without writing or deleting files.

## Minimal Example

```jsonc
{
  "id": "studio",
  "description": "Studio + Playground dev environment",
  "components": {
    "studio": {
      "path": "~/Developer/studio",
      "stack": "studio-combined"
    }
  },
  "resources": {
    "ports": [9724],
    "process_patterns": ["wordpress-server-child.mjs"]
  },
  "services": {
    "tarballs": {
      "kind": "http-static",
      "cwd": "${components.studio.path}/dist/packages-for-self-hosting",
      "port": 9724,
      "health": { "http": "http://127.0.0.1:9724/", "expect_status": 200 }
    },
    "studio-daemon": {
      "kind": "external",
      "discover": { "pattern": "wordpress-server-child.mjs" }
    }
  },
  "pipeline": {
    "up": [
      { "kind": "service", "id": "tarballs", "op": "start" },
      { "kind": "service", "id": "tarballs", "op": "health" }
    ],
    "check": [
      { "kind": "service", "id": "tarballs", "op": "health" },
      {
        "kind": "check",
        "label": "daemon newer than CLI bundle",
        "newer_than": {
          "left": { "process_start": { "pattern": "wordpress-server-child.mjs" } },
          "right": { "file_mtime": "${components.studio.path}/dist/cli/index.js" }
        }
      }
    ],
    "down": [
      { "kind": "service", "id": "studio-daemon", "op": "stop" },
      { "kind": "service", "id": "tarballs", "op": "stop" }
    ]
  }
}
```

## State

Rig runtime state lives at `~/.config/homeboy/rigs/<id>.state/`:

- `state.json` records service PIDs, shared-path ownership markers, and last `up` / `check` timestamps.
- `logs/<service-id>.log` captures stdout/stderr for rig-managed services.

Package source metadata lives next to Homeboy's rig and stack config directories so `rig update` and `rig sources` can tell which files are Homeboy-owned.

State is ephemeral. Deleting it makes the next command re-probe the environment; it is not the source of truth for the rig spec.

## See Also

- [rig-spec.md](./rig-spec.md) - full spec schema reference.
- [stack.md](./stack.md) - combined-fixes branch specs used by `rig sync`.
- [bench.md](./bench.md) - rig-pinned benchmark runs.
- [rig-matrix-axis-composition.md](../architecture/rig-matrix-axis-composition.md) - future design for derived rig variants.
- [fleet.md](./fleet.md) - remote multi-project equivalent; rigs are local, fleets are remote.
