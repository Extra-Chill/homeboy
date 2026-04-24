# Rig spec reference

JSON schema for `~/.config/homeboy/rigs/<id>.json`.

## Top-level fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | No | Rig ID. If absent, the filename stem (`<id>.json` → `<id>`) is used. |
| `description` | string | No | Human-readable description shown in `rig list` / `rig show`. |
| `components` | object | No | Map of component ID → `ComponentSpec`. |
| `services` | object | No | Map of service ID → `ServiceSpec`. |
| `symlinks` | array | No | List of `SymlinkSpec` entries. |
| `pipeline` | object | No | Map of pipeline name → array of `PipelineStep`. |

## `ComponentSpec`

Reference to a local checkout of a component. Decoupled from homeboy's global component registry so rigs work without prior `homeboy component create`.

| Field | Type | Description |
|---|---|---|
| `path` | string | Filesystem path to the checkout. Supports `~` and `${env.VAR}` expansion. |
| `stack` | string | Stack ID (Phase 2 — reserved, currently informational). |
| `branch` | string | Expected branch hint. Documentation only in MVP. |

## `ServiceSpec`

A background process the rig manages.

| Field | Type | Description |
|---|---|---|
| `kind` | enum | `"http-static"` or `"command"`. |
| `cwd` | string | Working directory. Supports variable expansion. |
| `port` | integer | TCP port. Required for `http-static`; surfaced in status for `command`. |
| `command` | string | Shell command for `kind: "command"`. |
| `env` | object | Env vars passed to the service. |
| `health` | `CheckSpec` | Health probe evaluated by `rig check`. Optional — missing means "alive PID = healthy". |

### Service kinds

- **`http-static`** — runs `python3 -m http.server <port>` in `cwd`. The common case for dev envs that need to serve tarballs or static assets locally.
- **`command`** — runs `sh -c <command>`. Use for anything else (docker, redis, custom dev servers, SSH tunnels).

Services are started detached (new session via `setsid`), tracked by PID in state, and logged to `~/.config/homeboy/rigs/<id>.state/logs/<service-id>.log`.

## `SymlinkSpec`

A symlink the rig maintains.

| Field | Type | Description |
|---|---|---|
| `link` | string | Path where the symlink lives. Supports `~` expansion. |
| `target` | string | What the symlink points to. Supports `~` expansion. |

`symlink ensure` creates or re-points the link; `symlink verify` checks it exists with the expected target.

## `PipelineStep`

Tagged union via the `kind` discriminator. Four shapes:

### `service`

```jsonc
{ "kind": "service", "id": "<service-id>", "op": "start" }
{ "kind": "service", "id": "<service-id>", "op": "stop" }
{ "kind": "service", "id": "<service-id>", "op": "health" }
```

`start` is idempotent — a running PID is reused. `stop` sends SIGTERM with 5s grace then SIGKILL. `health` evaluates the service's `health` check and verifies the PID is live.

### `command`

```jsonc
{
  "kind": "command",
  "command": "npx nx run-many --target=package-for-self-hosting",
  "cwd": "${components.wordpress-playground.path}",
  "env": { "NODE_ENV": "development" },
  "label": "build playground tarballs"
}
```

Runs via `sh -c`. `cwd`, `command`, and `env` values all support variable expansion. `label` is optional; without it, the command string itself is used in status output.

### `symlink`

```jsonc
{ "kind": "symlink", "op": "ensure" }
{ "kind": "symlink", "op": "verify" }
```

Operates on every symlink declared at the rig level. No per-step target — rig-wide intent.

### `check`

```jsonc
{
  "kind": "check",
  "label": "docker daemon running",
  "command": "docker info",
  "expect_exit": 0
}
```

Embeds a `CheckSpec` (see below). Non-fatal in `up` pipelines; fatal in `check` pipelines.

## `CheckSpec`

A single declarative probe. Exactly one of the three probe fields must be set.

### HTTP probe

```jsonc
{ "http": "http://127.0.0.1:9724/", "expect_status": 200 }
```

Issues a `GET` with a 5s timeout. Passes if status matches `expect_status` (default `200`).

### File probe

```jsonc
{ "file": "~/Studio/mysite/wp-content/db.php" }
{ "file": "~/Studio/mysite/wp-content/db.php", "contains": "Markdown Database Integration" }
```

Passes if the file exists. If `contains` is set, also requires the file contents to include that substring.

### Command probe

```jsonc
{ "command": "docker info", "expect_exit": 0 }
```

Runs via `sh -c`. Passes if exit code matches `expect_exit` (default `0`).

## Variable expansion

Three substitutions apply to `cwd`, `command`, `link`, `target`, and `CheckSpec` fields:

- `${components.<id>.path}` — component path from the rig spec
- `${env.<NAME>}` — process environment variable (empty if unset)
- `~` — home directory (standard tilde expansion)

Unknown `${...}` patterns are left literal so failures are loud and localized.

## Pipeline semantics

- `up` — fail-fast; aborts on first failing step, marks downstream steps as `skip`.
- `check` — runs every step regardless of failures so you see every problem at once.
- `down` — fail-fast through `down` pipeline, then stops every declared service unconditionally (belt + suspenders).

MVP pipelines are linear lists. DAG pipelines with cross-component dependencies + caching land in Phase 3 (tracked as Automattic/homeboy #1464).

## Example rigs

See [rig.md](./rig.md) for the studio-playground-dev example.
