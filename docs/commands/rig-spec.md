# Rig Spec Reference

JSON reference for rig specs loaded from `~/.config/homeboy/rigs/<id>.json` or installed from a rig package.

## Package Layout

A rig package can be a local directory or a git repository. Discovery accepts either shape:

```text
single-rig-package/
  rig.json

multi-rig-package/
  rigs/<rig-id>/rig.json
  stacks/<stack-id>.json
```

Install with:

```sh
homeboy rig install https://github.com/chubes4/homeboy-rigs.git//packages/studio --id studio
homeboy rig install ./packages/studio
homeboy rig install ./packages --all
```

For git sources, `repo.git//subpath` clones the repository root but discovers specs from the selected subpath. Installed source metadata records the package root, discovery path, linked/cloned ownership, and git revision so `rig update` and `rig sources` can refresh or remove the right files later.

## Top-Level Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | No | Rig ID. If absent, the filename stem is used. |
| `description` | string | No | Human-readable description shown in `rig list` / `rig show`. |
| `components` | object | No | Map of component ID to `ComponentSpec`. |
| `services` | object | No | Map of service ID to `ServiceSpec`. |
| `symlinks` | array | No | List of `SymlinkSpec` entries. |
| `shared_paths` | array | No | List of dependency paths the rig may borrow from another checkout. |
| `resources` | object | No | Resource declarations used by active-run leases. |
| `pipeline` | object | No | Map of pipeline name to `PipelineStep[]`. |
| `bench` | object | No | Rig-pinned benchmark component/default-baseline settings. |
| `bench_workloads` | object | No | Rig-owned out-of-tree benchmark workload paths keyed by extension ID. |
| `bench_profiles` | object | No | Named benchmark scenario profiles keyed by profile name. |
| `app_launcher` | object | No | Optional desktop launcher wrapper config. |

## `ComponentSpec`

Components are local checkouts used by pipeline steps. They are intentionally decoupled from the global component registry so package rigs can work on machines where the component has not been registered globally.

| Field | Type | Description |
|---|---|---|
| `path` | string | Filesystem path to the checkout. Supports `~`, `${env.NAME}`, and use through `${components.<id>.path}`. |
| `remote_url` | string | Optional source repository URL for triage/reporting fallback. |
| `triage_remote_url` | string | Optional reporting-only GitHub remote override. |
| `stack` | string | Stack ID synced by `homeboy rig sync` and by explicit `stack` pipeline steps. |
| `branch` | string | Expected branch hint surfaced to humans in status/spec output. |
| `extensions` | object | Optional rig-owned scoped extension config, mainly for rig-pinned bench dispatch. |

Example:

```jsonc
{
  "components": {
    "studio": {
      "path": "~/Developer/studio",
      "stack": "studio-combined",
      "branch": "dev/combined-fixes"
    }
  }
}
```

## `ServiceSpec`

Services are background processes the rig can start, stop, adopt, or health-check.

| Field | Type | Description |
|---|---|---|
| `kind` | enum | `http-static`, `command`, or `external`. |
| `cwd` | string | Working directory. Supports variable expansion. |
| `port` | integer | TCP port. Required for `http-static`; shown in status for `command`. |
| `command` | string | Shell command for `kind: "command"`. |
| `env` | object | Environment variables passed to the service. |
| `health` | `CheckSpec` | Optional health probe. Missing means a live PID is healthy. |
| `discover` | object | Required for `external`; contains `pattern` for process discovery. |

### Service Kinds

- **`http-static`** runs `python3 -m http.server <port>` in `cwd`.
- **`command`** runs `sh -c <command>` and tracks the spawned PID.
- **`external`** adopts a process Homeboy did not spawn. It discovers the newest process whose command line contains `discover.pattern`; `service.stop` signals it, while `service.start` intentionally errors.

```jsonc
{
  "services": {
    "tarballs": {
      "kind": "http-static",
      "cwd": "${components.playground.path}/dist/packages-for-self-hosting",
      "port": 9724,
      "health": { "http": "http://127.0.0.1:9724/", "expect_status": 200 }
    },
    "studio-daemon": {
      "kind": "external",
      "discover": { "pattern": "wordpress-server-child.mjs" }
    }
  }
}
```

Managed services are detached, tracked by PID in rig state, and logged under `~/.config/homeboy/rigs/<id>.state/logs/`.

## `resources` And Active-Run Leases

`resources` declares what a mutating rig command may exclusively touch while it is active. `rig up` and `rig down` acquire a local lease before running, prune stale leases, and fail with a resource-conflict error if another active rig command overlaps.

| Field | Type | Description |
|---|---|---|
| `exclusive` | array | Logical tokens that must not overlap with another active rig. |
| `paths` | array | Filesystem paths the rig mutates or requires exclusively. |
| `ports` | array | TCP ports the rig binds or assumes ownership of. |
| `process_patterns` | array | Process command-line substrings the rig may stop or inspect. |

```jsonc
{
  "resources": {
    "exclusive": ["studio-dev"],
    "paths": ["~/Studio/intelligence-chubes4/wp-content/plugins"],
    "ports": [9724],
    "process_patterns": ["wordpress-server-child.mjs"]
  }
}
```

Leases guard concurrent active commands; they are not long-lived ownership records after the command exits.

## `SymlinkSpec`

| Field | Type | Description |
|---|---|---|
| `link` | string | Path where the symlink lives. Supports `~`. |
| `target` | string | Expected symlink target. Supports `~`. |

`symlink ensure` creates or repoints the link. `symlink verify` checks that the link exists and points at the expected target.

## `SharedPathSpec`

Shared paths let worktrees borrow heavy dependency directories from another checkout.

| Field | Type | Description |
|---|---|---|
| `link` | string | Path inside the active checkout. |
| `target` | string | Existing path to borrow. |

Safety contract:

- `ensure` creates a symlink only when `link` is missing.
- Real files or directories at `link` are treated as local dependencies and left alone.
- A symlink at `link` pointing anywhere else is an error.
- `cleanup` removes only symlinks this rig created and recorded in rig state.

## Pipeline Steps

Pipeline steps are a tagged union via the `kind` field. Every step can include:

| Field | Type | Description |
|---|---|---|
| `id` | string | Optional stable step ID. |
| `depends_on` | array | Step IDs that must run first. |
| `label` | string | Optional human-readable status label where the step type supports it. |

Steps are ordered topologically by `depends_on`, then executed in order. Cycles and missing dependency IDs fail before running the pipeline. This gives dependency-aware ordering without making `up` / `check` / `down` parallel.

### `service`

```jsonc
{ "kind": "service", "id": "tarballs", "op": "start" }
{ "kind": "service", "id": "tarballs", "op": "health" }
{ "kind": "service", "id": "tarballs", "op": "stop" }
```

Starts, checks, or stops a declared service. `start` is idempotent for managed services. `stop` sends SIGTERM with a grace period and then SIGKILL if needed.

### `build`

```jsonc
{ "kind": "build", "component": "wordpress-playground", "label": "build tarballs" }
```

Delegates to `homeboy build` using the component path from the rig spec.

### `extension`

```jsonc
{ "kind": "extension", "component": "studio", "op": "build" }
```

Delegates a supported component lifecycle operation through extension infrastructure. Current rig support is `op: "build"`; use `command` for one-off escape hatches.

### `git`

```jsonc
{
  "kind": "git",
  "component": "studio",
  "op": "rebase",
  "args": ["--autostash", "origin/trunk"],
  "label": "rebase Studio"
}
```

Runs git in the component checkout. Supported operations are `status`, `pull`, `push`, `fetch`, `checkout`, `current-branch`, `rebase`, and `cherry-pick`. `args` are appended after the operation's base arguments and support variable expansion.

### `stack`

```jsonc
{ "kind": "stack", "component": "studio", "op": "sync", "dry_run": true }
```

Delegates to `homeboy stack sync <stack-id>` for the named component. The component must declare `stack`. `homeboy rig sync <id>` runs this for every stacked component without needing a pipeline step.

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

Applies or verifies an idempotent local-only patch. `marker` must appear in `content`; when the marker is already present, `apply` is a no-op. If `after` is set and not found, the step fails rather than guessing where to insert. Use `op: "verify"` in `check` pipelines.

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

Runs via the platform shell. `cwd`, `command`, and `env` values support variable expansion. Command steps bootstrap common developer-tool PATH entries unless `env.PATH` is set explicitly.

Prefer typed `build`, `git`, `stack`, `check`, and `service` steps when they fit; generic commands are the escape hatch.

### `symlink`

```jsonc
{ "kind": "symlink", "op": "ensure" }
{ "kind": "symlink", "op": "verify" }
```

Operates on every top-level `symlinks` entry.

### `shared-path`

```jsonc
{ "kind": "shared-path", "op": "ensure" }
{ "kind": "shared-path", "op": "verify" }
{ "kind": "shared-path", "op": "cleanup" }
```

Operates on every top-level `shared_paths` entry.

### `check`

```jsonc
{
  "kind": "check",
  "label": "docker daemon running",
  "command": "docker info",
  "expect_exit": 0
}
```

Embeds a `CheckSpec`. `up` pipelines fail fast on step failures; `check` pipelines run all steps and report every failure.

## `CheckSpec`

Exactly one probe field should be set.

### HTTP Probe

```jsonc
{ "http": "http://127.0.0.1:9724/", "expect_status": 200 }
```

Issues a `GET` with a 5s timeout. `expect_status` defaults to `200`.

### File Probe

```jsonc
{ "file": "~/Studio/mysite/wp-content/db.php" }
{ "file": "~/Studio/mysite/wp-content/db.php", "contains": "Markdown Database Integration" }
```

Checks that the file exists and optionally contains a substring.

### Command Probe

```jsonc
{ "command": "docker info", "expect_exit": 0 }
```

Runs via the shell. `expect_exit` defaults to `0`.

### Staleness Probe

```jsonc
{
  "newer_than": {
    "left": { "process_start": { "pattern": "wordpress-server-child.mjs" } },
    "right": { "file_mtime": "${components.studio.path}/dist/cli/index.js" }
  }
}
```

Passes when `left` is newer than `right`. Each side chooses one time source: `file_mtime` or `process_start`. If the left side is `process_start` and no matching process exists, the check passes because there is no stale process to flag. Other missing sources are errors.

## Bench Fields

Rig specs can pin benchmark dispatch for `homeboy bench --rig <id>`.

| Field | Type | Description |
|---|---|---|
| `bench.default_component` | string | Component to benchmark when no component is passed. |
| `bench.components` | array | Components to benchmark as a rig-pinned matrix. |
| `bench.default_baseline_rig` | string | Implicit baseline rig for branch-vs-main comparisons. |
| `bench.warmup_iterations` | integer | Warmup iterations forwarded to bench runners. |
| `bench_workloads` | object | Out-of-tree workload paths keyed by extension ID. |
| `bench_profiles` | object | Named scenario lists used by `homeboy bench --profile <name>`. |

`bench_workloads` paths support `~`, `${env.NAME}`, `${components.<id>.path}`, and `${package.root}` for package-installed rigs.

```jsonc
{
  "bench": {
    "components": ["studio", "playground"],
    "default_baseline_rig": "studio-main",
    "warmup_iterations": 2
  },
  "bench_workloads": {
    "wordpress": ["${package.root}/bench/workloads/studio-cold-start"]
  },
  "bench_profiles": {
    "cold-start": ["admin-first-load", "site-editor-first-load"]
  }
}
```

## `app_launcher`

`app_launcher` config powers `homeboy rig app install|update|uninstall`.

| Field | Type | Description |
|---|---|---|
| `platform` | enum | Currently `macos`. |
| `wrapper_display_name` | string | Display name for the generated `.app` bundle. |
| `wrapper_bundle_id` | string | Bundle identifier written to `Info.plist`. |
| `target_app` | string | App or executable opened after rig prep succeeds. |
| `install_dir` | string | Optional install directory; defaults to `/Applications`. |
| `preflight` | array | Preflight actions; defaults to `rig:check`. |
| `on_preflight_fail` | string | Optional failure behavior for generated launcher scripts. |

```jsonc
{
  "app_launcher": {
    "platform": "macos",
    "wrapper_display_name": "Studio Dev",
    "wrapper_bundle_id": "dev.homeboy.studio",
    "target_app": "${components.studio.path}/dist/mac/Studio.app",
    "preflight": ["rig:check"]
  }
}
```

## Variable Expansion

Common path/string fields support:

- `${components.<id>.path}` for component paths from the rig spec.
- `${env.NAME}` for process environment variables; unset values expand to empty strings.
- `~` for the current user's home directory.

Rig-owned benchmark workload paths also support `${package.root}` when the rig was installed from a package source.

Unknown `${...}` patterns are left literal so the eventual command or file check fails loudly.

## Pipeline Semantics

- `up` runs the `up` pipeline with fail-fast behavior.
- `check` runs every step in the `check` pipeline and reports all failures.
- `down` runs the `down` pipeline, then stops declared services and cleans rig-owned shared paths as a safety net.
- Dependency edges from `depends_on` are resolved before execution; the executor still runs one ordered list, not parallel jobs.
- Stack synchronization is explicit through `homeboy rig sync` or `kind: "stack"` steps. `rig up` does not sync stacks unless the spec author adds that step.

## Future Work

The shipped rig lifecycle is local/package/source/stack/app capable. The remaining design work is matrix/axis composition and build DAG caching for derived rig variants, tracked separately from the core rig command reference.

## See Also

- [rig.md](./rig.md) - command lifecycle and examples.
- [stack.md](./stack.md) - stack specs consumed by `rig sync`.
- [bench.md](./bench.md) - benchmark command behavior for rig-pinned runs.
- [rig-matrix-axis-composition.md](../architecture/rig-matrix-axis-composition.md) - future matrix/axis design.
