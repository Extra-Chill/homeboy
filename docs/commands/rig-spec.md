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
| `shared_paths` | array | No | List of ephemeral dependency paths to borrow from another checkout. |
| `pipeline` | object | No | Map of pipeline name → array of `PipelineStep`. |

## `ComponentSpec`

Reference to a local checkout of a component. Decoupled from homeboy's global component registry so rigs work without prior `homeboy component create`.

| Field | Type | Description |
|---|---|---|
| `path` | string | Filesystem path to the checkout. Supports `~` and `${env.VAR}` expansion. |
| `stack` | string | Stack ID associated with the component. Used by `homeboy stack`; rig pipeline integration is still explicit. |
| `branch` | string | Expected branch hint for humans reading specs and status output. |

## `ServiceSpec`

A background process the rig manages.

| Field | Type | Description |
|---|---|---|
| `kind` | enum | `"http-static"`, `"command"`, or `"external"`. |
| `cwd` | string | Working directory. Supports variable expansion. |
| `port` | integer | TCP port. Required for `http-static`; surfaced in status for `command`. |
| `command` | string | Shell command for `kind: "command"`. |
| `env` | object | Env vars passed to the service. |
| `health` | `CheckSpec` | Health probe evaluated by `rig check`. Optional — missing means "alive PID = healthy". |

### Service kinds

- **`http-static`** — runs `python3 -m http.server <port>` in `cwd`. The common case for dev envs that need to serve tarballs or static assets locally.
- **`command`** — runs `sh -c <command>`. Use for anything else (docker, redis, custom dev servers, SSH tunnels).
- **`external`** — adopts a process the rig did not spawn. Only `service.stop` is meaningful; it discovers the newest process whose command line contains `discover.pattern` and signals it. Use for stale daemons that need recycling after a build.

Services are started detached (new session via `setsid`), tracked by PID in state, and logged to `~/.config/homeboy/rigs/<id>.state/logs/<service-id>.log`.

## `SymlinkSpec`

A symlink the rig maintains.

| Field | Type | Description |
|---|---|---|
| `link` | string | Path where the symlink lives. Supports `~` expansion. |
| `target` | string | What the symlink points to. Supports `~` expansion. |

`symlink ensure` creates or re-points the link; `symlink verify` checks it exists with the expected target.

## `SharedPathSpec`

An ephemeral dependency path the rig may borrow from another checkout. Common use: feature worktrees that do not have `node_modules`, while the primary checkout does.

| Field | Type | Description |
|---|---|---|
| `link` | string | Path inside the active checkout. If missing, `shared-path ensure` creates a symlink here. Supports variable expansion. |
| `target` | string | Existing path to borrow. Supports variable expansion. |

Safety contract:

- `ensure` creates a symlink only when `link` is missing.
- If `link` already exists as a real file or directory, it is treated as local dependencies and left alone.
- If `link` is a symlink to any other target, `ensure` fails instead of replacing it.
- Cleanup removes only symlinks this rig created and recorded in rig state.

## `PipelineStep`

Tagged union via the `kind` discriminator. **Prefer the typed primitives (`build`, `git`, `check`, `shared-path`) over generic `command` wherever they fit — typed steps reuse homeboy's existing build/git plumbing, error mapping, and extension hooks rather than shelling out blindly.**

### `service`

```jsonc
{ "kind": "service", "id": "<service-id>", "op": "start" }
{ "kind": "service", "id": "<service-id>", "op": "stop" }
{ "kind": "service", "id": "<service-id>", "op": "health" }
```

`start` is idempotent — a running PID is reused. `stop` sends SIGTERM with 5s grace then SIGKILL. `health` evaluates the service's `health` check and verifies the PID is live.

### `build`

```jsonc
{ "kind": "build", "component": "wordpress-playground", "label": "playground tarballs" }
```

Delegates to `homeboy build`, using the component's path from the rig's `components` map. The component does NOT need to be registered in homeboy's global component registry — path override wins. Build semantics (extension-registered build scripts, error formatting, structured result) are inherited from `homeboy::build::run_with_path`. Exit code non-zero fails the step with the stderr tail in the pipeline outcome.

### `git`

```jsonc
{
  "kind": "git",
  "component": "studio",
  "op": "pull",
  "args": ["origin", "trunk"],
  "label": "pull studio trunk"
}
```

Delegates to homeboy's git primitive (`git::execute_git_for_release` under the hood). Operation set: `status`, `pull`, `push`, `fetch`, `checkout`, `current-branch`, `rebase`, and `cherry-pick`. `args` are appended after the op-specific base args, so `op: pull` with `args: ["origin", "trunk"]` runs `git pull origin trunk`. Variable expansion applies to `args` entries.

For long-lived combined-fixes branches, prefer a `homeboy stack` spec plus explicit `stack` invocations over hand-maintaining a long sequence of raw cherry-pick steps in the rig.

### `patch`

```jsonc
{
  "kind": "patch",
  "component": "wordpress-playground",
  "file": "packages/php-wasm/compile/php/patches/local.h",
  "marker": "TSRMLS_CC fallback",
  "after": "#include <php.h>",
  "content": "/* TSRMLS_CC fallback */\n#ifndef TSRMLS_CC\n#define TSRMLS_CC\n#endif",
  "op": "apply",
  "label": "local TSRMLS fallback"
}
```

Applies or verifies an idempotent local-only file patch. `marker` must appear in `content`; if the marker is already present, `apply` is a no-op. If `after` is set and missing from the file, the step fails instead of guessing where to insert. Use `op: "verify"` in `check` pipelines.

### `command`

```jsonc
{
  "kind": "command",
  "command": "sleep 2",
  "cwd": "${components.wordpress-playground.path}",
  "env": { "NODE_ENV": "development" },
  "label": "wait for tarballs"
}
```

Runs via `sh -c`. `cwd`, `command`, and `env` values all support variable expansion. `label` is optional; without it, the command string itself is used in status output.

**Escape hatch — use sparingly.** If a step maps to `build`, `git`, or `check`, use those instead. Generic commands lose homeboy's error formatting, extension integration, and structured output.

### `symlink`

```jsonc
{ "kind": "symlink", "op": "ensure" }
{ "kind": "symlink", "op": "verify" }
```

Operates on every symlink declared at the rig level. No per-step target — rig-wide intent.

### `shared-path`

```jsonc
{ "kind": "shared-path", "op": "ensure" }
{ "kind": "shared-path", "op": "verify" }
{ "kind": "shared-path", "op": "cleanup" }
```

Operates on every `shared_paths` entry declared at the rig level. Use `ensure` before dependency-consuming commands, `verify` in `check`, and `cleanup` in `down` when you want explicit teardown. `rig down` also runs shared-path cleanup as a safety net.

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

A single declarative probe. Exactly one of the four probe fields must be set.

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

### Staleness probe

```jsonc
{
  "newer_than": {
    "left": { "process_start": "wordpress-server-child.mjs" },
    "right": { "file_mtime": "${components.studio.path}/dist/cli/index.js" }
  }
}
```

Passes when `left` is newer than `right`. Each side chooses one time source: `file_mtime` or `process_start`. If the left side is a `process_start` source and no matching process exists, the check passes because there is no stale process to flag. Other missing sources are errors.

## Variable expansion

Three substitutions apply to `cwd`, `command`, `link`, `target`, shared paths, and `CheckSpec` fields:

- `${components.<id>.path}` — component path from the rig spec
- `${env.<NAME>}` — process environment variable (empty if unset)
- `~` — home directory (standard tilde expansion)

Unknown `${...}` patterns are left literal so failures are loud and localized.

## Pipeline semantics

- `up` — fail-fast; aborts on first failing step, marks downstream steps as `skip`.
- `check` — runs every step regardless of failures so you see every problem at once.
- `down` — fail-fast through `down` pipeline, then stops every declared service unconditionally (belt + suspenders).

Pipelines are linear lists. DAG pipelines with cross-component dependencies + caching are still a future phase (tracked as Extra-Chill/homeboy #1464).

## Example rigs

See [rig.md](./rig.md) for the studio-playground-dev example.
