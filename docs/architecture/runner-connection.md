# Runner Connection Bootstrap

`homeboy runner connect <runner-id>` uses the first-class runner registry added
for issue #2526.

## Registry Contract

Runner configs are JSON files at `~/.config/homeboy/runners/<id>.json` with the
initial #2526 shape:

```json
{
  "kind": "ssh",
  "server_id": "lab-box",
  "workspace_root": "/srv/homeboy-lab",
  "homeboy_path": "homeboy",
  "daemon": true
}
```

Only `kind: "ssh"`, `server_id`, and optional `homeboy_path` are used by the
Wave 1 connection commands. Registry CRUD owns creation, validation, and future
execution metadata such as `workspace_root`, `concurrency_limit`, `env`, and
`resources`.

## Connection Shape

The remote daemon is started with `homeboy daemon start --addr 127.0.0.1:0` and
the reported address is rejected unless it is loopback. The local client reaches
the daemon through an SSH `-L 127.0.0.1:<local>:127.0.0.1:<remote>` tunnel.

Session metadata is stored at `~/.config/homeboy/runner-sessions/<id>.json` so
`status` and `disconnect` can inspect or close the local tunnel later.

## Rolling Generations

The runner-layer rolling-generation primitive models daemon replacement as
generations. A validated candidate starts on its own endpoint before it becomes
the admission owner. Existing jobs remain owned by the draining generation,
including their logs, cancellation, artifacts, and reconciliation records. A
draining generation retires only after its authoritative active-job count reaches
zero.

Its serializable status names the admission owner and each generation's endpoint,
active-job count, and `admitting` or `draining` state. A failed candidate startup
is removed without changing the prior owner; repeating a refresh for the active
generation is idempotent. Older single-generation records remain valid as one
admitting generation.
