# `homeboy runner`

## Synopsis

```sh
homeboy runner <COMMAND>
```

`runner` manages durable execution backends. SSH runners are a capability on a `homeboy server` record, so the common Lab flow uses one ID for the machine and its runner. Local runners remain standalone because they describe this machine rather than an SSH server.

## Subcommands

### `add`

```sh
homeboy runner add <id> --workspace-root <path>
homeboy runner add <server-id> --server <server-id> --workspace-root <path>
homeboy runner add --json <spec>
```

Options:

- `--kind local|ssh`: explicit runner kind. Defaults to `ssh` when `--server` is set, otherwise `local`.
- `--server <server-id>`: existing `homeboy server` record for SSH runners. For SSH runners, `<id>` must match `<server-id>`.
- `--workspace-root <path>`: workspace root on the runner machine.
- `--homeboy-path <path>`: Homeboy binary path on the runner machine.
- `--daemon`: marks the runner as daemon-preferred for future commands.
- `--concurrency-limit <n>`: maximum concurrent workflows this runner should accept.
- `--artifact-policy <label>`: artifact policy label reserved for future execution commands.

### `enable`

```sh
homeboy runner enable <server-id> --workspace-root <path>
homeboy runner enable <server-id> --workspace-root <path> --concurrency-limit 4 --artifact-policy copy
```

Enables runner capability on an existing SSH server. This is the recommended Homeboy Lab onboarding path:

```sh
homeboy server create homeboy-lab --host 192.168.86.63 --user chubes --port 22
homeboy runner enable homeboy-lab --workspace-root /home/chubes/Developer --concurrency-limit 4 --artifact-policy copy
homeboy runner connect homeboy-lab
```

After this, `homeboy-lab` is both the server ID and the runner ID.

### `migrate`

```sh
homeboy runner migrate <legacy-runner-id>
homeboy runner migrate <legacy-runner-id> --remove-legacy
```

Migrates a pre-capability standalone SSH runner onto the server referenced by its `server_id`. Use this for old Lab configs such as runner `lab` pointing at server `homeboy-lab`:

```sh
homeboy runner migrate lab
homeboy runner show homeboy-lab
homeboy runner migrate lab --remove-legacy
```

The migration copies `workspace_root`, `homeboy_path`, `daemon`, `concurrency_limit`, `artifact_policy`, `env`, and `resources` into the server's embedded `runner` capability. Without `--remove-legacy`, the old `~/.config/homeboy/runners/<legacy-runner-id>.json` file is preserved so the result can be inspected first. With `--remove-legacy`, Homeboy deletes the legacy standalone runner after the server capability has been saved.

### `doctor`

```sh
homeboy runner doctor local
homeboy runner doctor <runner-id>
```

Diagnoses a local or configured SSH runner without mutating it. Use `local`,
`localhost`, or `self` to inspect this machine without creating a runner record.
The JSON payload uses `command: "runner.doctor"` and includes `runner_id`,
`status`, `capabilities`, and warning/error details when a capability probe fails.

Use `doctor` before `connect` when you need to know whether Homeboy, Git, SSH,
and the configured workspace root are usable on the target machine.

Hot-command Lab offload uses the same capability vocabulary before running on
an explicit `--runner`. Homeboy currently gates `lint`, `test`, `audit`,
`bench`, and `trace` against the source worktree's lightweight tool signals:

- `package.json` requires `node` and `npm`.
- `pnpm-lock.yaml` requires `node` and `pnpm`.
- `composer.json` requires `php` and `composer`.
- Docker/Compose files require `docker`.
- `trace` requires Playwright plus browser binaries.

When an explicit runner is missing required tools, the command fails before
workspace sync with a `runner_capabilities` validation error and remediation.
The same central policy returns a local fallback reason for future automatic
Lab offload selection.

### `connect`

```sh
homeboy runner connect <runner-id>
```

Starts a loopback-only Homeboy daemon on the runner and opens an SSH tunnel to
it. This is the preferred Lab execution path because later `runner exec` calls
can use the daemon session instead of ad-hoc SSH command execution. The JSON
payload uses `command: "runner.connect"` and reports connection state such as
the runner ID, tunnel endpoint, daemon endpoint, and persisted session metadata.

### `status`

```sh
homeboy runner status <runner-id>
```

Shows the persisted tunnel/session state for a runner. Use this to determine
whether `runner exec` will use a connected daemon or needs an explicit fallback.
The JSON payload uses `command: "runner.status"` and reports whether a saved
session exists, whether the tunnel still appears live, and the recorded endpoint
details.

### `disconnect`

```sh
homeboy runner disconnect <runner-id>
```

Closes a persisted runner tunnel session and removes its local session state.
The JSON payload uses `command: "runner.disconnect"` and reports which session
state was removed. This is safe to run when no live session exists; it is the
explicit cleanup counterpart to `runner connect`.

### `list`

```sh
homeboy runner list
```

### `show`

```sh
homeboy runner show <id>
```

### `set`

```sh
homeboy runner set <id> --json <JSON>
homeboy runner set <id> workspace_root=/srv/homeboy
homeboy runner set <id> -- --concurrency_limit 4
```

Updates a runner by merging a JSON object into the runner config. SSH runner settings live under `servers/<id>.json` as the server's `runner` capability; local runners live under `runners/<id>.json`.

### `remove`

```sh
homeboy runner remove <id>
```

### `exec`

```sh
homeboy runner exec <runner-id> -- <command...>
homeboy runner exec <runner-id> --cwd /runner/workspace/project -- <command...>
homeboy runner exec <runner-id> --ssh --cwd /runner/workspace/project -- <command...>
```

`exec` submits the command to the connected runner daemon when `homeboy runner connect <runner-id>` has established a live loopback tunnel. If no daemon session is connected, local runners execute directly and SSH runners require explicit `--ssh`.

Path rules:

- SSH runners require `workspace_root` so local paths are not silently reused remotely.
- SSH `--cwd` must be an absolute path under the configured `workspace_root`.
- Omitting `--cwd` on an SSH runner uses the runner `workspace_root`.
- `--ssh` is the explicit diagnostic fallback when `connect` is unavailable; daemon execution is preferred because it records job metadata and supports artifact-oriented workflows.

### `workspace sync`

```sh
homeboy runner workspace sync <runner-id> --path <local-worktree>
homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot
homeboy runner workspace sync <runner-id> --path <local-worktree> --mode git
```

`workspace sync` materializes a laptop worktree under the runner's configured `workspace_root` so Lab execution can run against an explicit remote path while Git operations and canonical edits stay local.

Modes:

- `snapshot` copies the current local tree, including dirty edits, through a tar stream.
- `git` requires a clean local tree, then clones or refreshes `remote.origin.url` on the runner and checks out local `HEAD` detached.

Safety rules:

- The remote path is deterministic and lives under `<workspace_root>/_lab_workspaces/`.
- Snapshot sync excludes dependency directories, build outputs, caches, `.git`, and common secret file patterns such as `.env*`, `*.pem`, and `*.key`.
- Output includes `local_path`, `remote_path`, `sync_mode`, `snapshot_identity`, and snapshot `files` / `bytes` when available.
- The runner workspace is execution-only; this command does not push branches, commit, or make the runner authoritative for source changes.

### `workspace apply`

```sh
homeboy runner workspace apply <lab-apply.json>
homeboy runner workspace apply <lab-apply.json> --force
```

`workspace apply` brings a Lab-generated fix artifact back to the local source worktree recorded in the artifact's `source_snapshot.local_path`. It is local-only: it does not commit, push, or make the Lab runner canonical. Reviewability stays in normal local Git via `git status` and `git diff`.

Safety rules:

- The artifact must identify the local source worktree through `source_snapshot.local_path`.
- Homeboy recalculates the current local `source_snapshot.snapshot_hash` before applying.
- If the local source worktree drifted since the Lab snapshot, apply is refused unless `--force` is explicit.
- Unified diffs are checked with `git apply --check` before mutation, so conflicts do not partially apply.
- Delta paths must be relative and stay inside the source worktree.
- Output includes `apply_status`, `modified_files`, `expected_snapshot_hash`, and `current_snapshot_hash`.

Temporary Wave 4 adapter contract, until the Lab fix-capture contract settles:

```json
{
  "source_snapshot": {
    "runner_id": "lab-a",
    "local_path": "/Users/chubes/Developer/project@branch",
    "remote_path": "/srv/homeboy/_lab_workspaces/project-abc123",
    "git_sha": "...",
    "dirty": false,
    "sync_mode": "snapshot",
    "snapshot_hash": "sha256:...",
    "synced_at": "2026-05-16T00:00:00Z",
    "sync_excludes": [".git/", "node_modules/"]
  },
  "patch": {
    "format": "unified_diff",
    "content": "diff --git a/file.txt b/file.txt\n..."
  }
}
```

Delta form is also accepted for explicit file replacement/deletion:

```json
{
  "source_snapshot": { "...": "..." },
  "delta": {
    "files": [
      { "path": "src/file.txt", "content_base64": "Li4u" },
      { "path": "obsolete.txt", "delete": true }
    ]
  }
}
```

## Runner Shape

SSH runner records are stored on their server as `runner` capability config under `~/.config/homeboy/servers/<id>.json`.

```json
{
  "id": "homeboy-lab",
  "host": "192.168.86.63",
  "user": "chubes",
  "port": 22,
  "runner": {
    "workspace_root": "/home/chubes/Developer",
    "homeboy_path": "/usr/local/bin/homeboy",
    "daemon": false,
    "concurrency_limit": 4,
    "artifact_policy": "copy",
    "env": {},
    "resources": {}
  }
}
```

Standalone local runner records are still stored under `~/.config/homeboy/runners/`.

```json
{
  "id": "lab-local",
  "kind": "local",
  "server_id": null,
  "workspace_root": "/Users/chubes/Developer",
  "homeboy_path": "/usr/local/bin/homeboy",
  "daemon": false,
  "concurrency_limit": 2,
  "artifact_policy": "copy",
  "env": {},
  "resources": {}
}
```

Rules:

- `kind` is `local` or `ssh`.
- `ssh` runner IDs are server IDs; a single SSH machine does not need a separate runner ID.
- `concurrency_limit`, when set, must be greater than zero.
- `env` and `resources` are metadata maps for future `connect`, `doctor`, `exec`, and Desktop workflows.

## JSON Output

All command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). The `data` payload uses the generic entity CRUD shape:

- `command`: action identifier such as `runner.add`, `runner.enable`, `runner.migrate`, `runner.list`, `runner.show`, `runner.set`, `runner.remove`, `runner.doctor`, `runner.connect`, `runner.status`, `runner.disconnect`, `runner.exec`, `runner.workspace.sync`, or `runner.workspace.apply`
- `id`: present for single-runner actions
- `entity`: runner configuration for single-runner read/write actions
- `entities`: list for `list`
- `updated_fields`: list of updated field names for writes
- `deleted`: list of removed runner IDs
