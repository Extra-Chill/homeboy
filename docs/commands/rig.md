# `homeboy rig`

Manage local dev **rigs** — code-defined, reproducible multi-component development environments.

## Synopsis

```sh
homeboy rig <COMMAND>
```

## Overview

A rig is a named bundle of components, local services, shared dependency paths, pre-flight checks, and a linear build pipeline, described as JSON at `~/.config/homeboy/rigs/<id>.json`. `rig up` materializes the env; `rig check` reports health; `rig down` tears it down.

Rigs are the missing piece between individual components (one repo, one version) and full deployments (many repos, remote servers) — they capture the setup a dev environment needs: which commits of which components, which background services are running, which pre-flight invariants must hold.

**Typical consumer:** a cross-repo setup that today lives as a wiki runbook (Studio + Playground with combined-fixes, WordPress core + Gutenberg dev, a sandbox + tunnel, etc).

**Current scope:** linear pipelines, `http-static`, `command`, and `external` service kinds, shared dependency paths, idempotent local patch steps, typed git/build/check primitives, and `up` / `check` / `down` / `status` / `list` / `show` verbs. See Extra-Chill/homeboy #1461 for the broader phased roadmap (DAG pipelines, extension-registered service kinds, `.app` wrappers, bench composition, spec sharing).

## Subcommands

### `list`

```sh
homeboy rig list
```

List all rigs declared in `~/.config/homeboy/rigs/`.

### `show`

```sh
homeboy rig show <id>
```

Print the full rig spec as JSON.

### `up`

```sh
homeboy rig up <id>
```

Run the rig's `up` pipeline: start services, run commands, ensure symlinks, evaluate checks. Idempotent — already-running services are left alone. Exits non-zero if any pipeline step fails.

### `check`

```sh
homeboy rig check <id>
```

Run the rig's `check` pipeline (health probes, file-existence checks, HTTP probes). Does NOT fail-fast: every failing check is reported so you can fix the env in one pass.

### `down`

```sh
homeboy rig down <id>
```

Stop every service the rig manages and run the `down` pipeline if defined.

### `status`

```sh
homeboy rig status <id>
```

Report current state: running services with PIDs, timestamps for last `up` / `check`.

## Rig spec format

See [rig-spec.md](./rig-spec.md) for the full schema. Minimal example:

```jsonc
{
  "id": "studio-playground-dev",
  "description": "Dev Studio + Playground with combined-fixes",

  "services": {
    "tarball-server": {
      "kind": "http-static",
      "cwd": "~/Developer/wordpress-playground/dist/packages-for-self-hosting",
      "port": 9724,
      "health": { "http": "http://127.0.0.1:9724/", "expect_status": 200 }
    }
  },

  "symlinks": [
    { "link": "~/.local/bin/studio", "target": "~/.local/bin/studio-dev" }
  ],

  "shared_paths": [
    {
      "link": "${components.studio.path}/node_modules",
      "target": "~/Developer/studio/node_modules"
    }
  ],

  "pipeline": {
    "up":    [
      { "kind": "service", "id": "tarball-server", "op": "start" },
      { "kind": "symlink", "op": "ensure" },
      { "kind": "shared-path", "op": "ensure" }
    ],
    "check": [
      { "kind": "service", "id": "tarball-server", "op": "health" },
      { "kind": "symlink", "op": "verify" },
      { "kind": "shared-path", "op": "verify" },
      {
        "kind": "check",
        "label": "MDI drop-in intact",
        "file": "~/Studio/mysite/wp-content/db.php",
        "contains": "Markdown Database Integration"
      }
    ],
    "down":  [
      { "kind": "shared-path", "op": "cleanup" },
      { "kind": "service", "id": "tarball-server", "op": "stop" }
    ]
  }
}
```

## State

Rig runtime state lives at `~/.config/homeboy/rigs/<id>.state/`:

- `state.json` — service PIDs, shared-path ownership markers, last `up`/`check` timestamps
- `logs/<service-id>.log` — captured stdout/stderr per service

State is ephemeral — deleting it means `rig up` will re-probe on next invocation. Never treat it as source of truth.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Pipeline succeeded |
| 1 | Pipeline had at least one failing step |
| 4 | Rig not found (`rig.not_found`) |
| 20 | Service or pipeline operational failure |

## See also

- [rig-spec.md](./rig-spec.md) — full spec schema reference
- [stack.md](./stack.md) — combined-fixes branch specs that rigs can reference
- [fleet.md](./fleet.md) — remote multi-project equivalent (rigs are local, fleets are remote)
- Extra-Chill/homeboy #1461 — design + phased roadmap
